//! Java-owned DAP policy adapted from Nanobug Rust; MIT notice is in licenses/nanobug-rust-LICENSE.txt.
mod inspection;
mod messages;

use super::transport::{Transport, failure};
use plugin_protocol::{
    api::{self, ErrorCode, Failure, ResourceHandle},
    process, service,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

/// What one DAP response completes; internal handshake exchanges have no public reply handle.
enum Pending {
    Freeze,
    Inspection(inspection::Request),
    Initialize,
    Launch,
    InitialBreakpoints,
    Configured,
    Location(u64),
    Control {
        reply: Option<ResourceHandle>,
        moves: bool,
        pause: u64,
    },
    Stop(Option<ResourceHandle>),
    Breakpoints {
        reply: ResourceHandle,
        groups: VecDeque<(String, Vec<u32>)>,
        values: Vec<Value>,
        source: String,
    },
    Frames(ResourceHandle, u64),
    Scopes(ResourceHandle, u64),
    Variables {
        reply: ResourceHandle,
        pause: u64,
        references: VecDeque<u64>,
        values: Vec<Value>,
    },
}

/// All mutable adapter state stays in this guest; the handle is a host-owned native job root.
pub struct Session {
    /// Every variable reference is issued only after a real frame report for its thread and pause.
    inspection: inspection::Inspection,
    pub handle: ResourceHandle,
    /// A separate owned PTY transports target I/O; adapter stdout remains byte-exact DAP.
    terminal: Option<ResourceHandle>,
    pub owner: String,
    id: String,
    transport: Transport,
    pending: BTreeMap<u32, Pending>,
    launch: Value,
    start_reply: Option<ResourceHandle>,
    breakpoints: VecDeque<(String, Vec<u32>)>,
    configured: bool,
    launched: bool,
    state: &'static str,
    thread: u64,
    /// Preserve the requested thread when the adapter reports that every thread was suspended.
    pausing: Option<u64>,
    /// Java initially stops the event thread; publish a pause only after the actual all-thread stop.
    freezing: Option<String>,
    /// A pending motion blocks new inspections, while preserving the last observed pause on failure.
    moving: Option<u64>,
    pause: u64,
    reason: String,
    source: Option<String>,
    line: Option<u32>,
    diagnostics: String,
    stop_replies: Vec<ResourceHandle>,
}

impl Session {
    /// Start only the installation-approved service; launch the target through DAP exactly once.
    pub fn start(id: String, call: service::Invocation) -> Result<Self, Failure> {
        let reply = call
            .reply
            .clone()
            .ok_or_else(|| failure("plugin.services 1.1 is required"))?;
        let (handle, launch) = super::launch::start(&id, &call)?;
        let mut session = Self {
            inspection: inspection::Inspection::default(),
            handle,
            terminal: None,
            owner: call.caller.instance,
            id,
            transport: Transport::default(),
            pending: BTreeMap::new(),
            launch,
            start_reply: Some(reply),
            breakpoints: VecDeque::new(),
            configured: false,
            launched: false,
            state: "starting",
            thread: 0,
            pausing: None,
            freezing: None,
            moving: None,
            pause: 0,
            reason: String::new(),
            source: None,
            line: None,
            diagnostics: String::new(),
            stop_replies: Vec::new(),
        };
        session.breakpoints = group_breakpoints(&session.launch)?;
        if let Err(error) = session.send("initialize", json!({"adapterID":"java","clientID":"me-editor","linesStartAt1":true,"columnsStartAt1":true,"pathFormat":"path","supportsRunInTerminalRequest":true,"supportsVariablePaging":true}), Pending::Initialize) {
            session.release(); return Err(error);
        }
        Ok(session)
    }
    /// Native completion is authoritative even if the adapter never sent a DAP terminated event.
    pub fn ended(&self) -> bool {
        self.state == "exited"
    }
    /// Exact source ownership was checked by the dispatcher before every control or read.
    pub fn call(&mut self, call: service::Invocation) -> Result<Option<Value>, Failure> {
        if call.method == "status" {
            return Ok(Some(self.status()));
        }
        if self.ended() {
            return if call.method == "stop" {
                Ok(Some(self.receipt()))
            } else {
                Err(Failure::new(ErrorCode::InvalidState, "Debug target ended"))
            };
        }
        if self.pending.len() >= 32 {
            return Err(Failure::new(
                ErrorCode::LimitExceeded,
                "Debug request quota exceeded",
            ));
        }
        let reply = call
            .reply
            .clone()
            .ok_or_else(|| failure("Missing deferred reply"))?;
        match call.method.as_str() {
            "threads" | "thread_frames" | "variable_children" => self.inspect(call)?,
            "stop" => {
                self.send(
                    "disconnect",
                    json!({"terminateDebuggee":true}),
                    Pending::Stop(Some(reply)),
                )?;
            }
            "resume" | "step" => {
                self.require_pause(&call.arguments)?;
                let command = if call.method == "resume" {
                    "continueAll"
                } else {
                    match call.arguments["kind"].as_str() {
                        Some("in" | "into" | "step_in") => "stepIn",
                        Some("over" | "step_over") => "next",
                        Some("out" | "step_out") => "stepOut",
                        _ => {
                            return Err(Failure::new(
                                ErrorCode::InvalidRequest,
                                "Unknown step kind",
                            ));
                        }
                    }
                };
                // A rejected or unsent command did not move the target. Only an acknowledged
                // motion or an actual continued/stopped event changes its published pause epoch.
                self.send(
                    command,
                    json!({"threadId":self.thread}),
                    Pending::Control {
                        reply: Some(reply),
                        moves: true,
                        pause: self.pause,
                    },
                )?;
                self.moving = Some(self.pause);
            }
            "pause" => {
                if self.state != "running" {
                    return Err(Failure::new(
                        ErrorCode::InvalidState,
                        "Target is not running",
                    ));
                }
                self.send(
                    "pause",
                    json!({"threadId":self.thread}),
                    Pending::Control {
                        reply: Some(reply),
                        moves: false,
                        pause: self.pause,
                    },
                )?;
                self.pausing = Some(self.thread);
            }
            "frames" => {
                self.require_pause(&call.arguments)?;
                self.send(
                    "stackTrace",
                    json!({"threadId":self.thread,"startFrame":0,"levels":256}),
                    Pending::Frames(reply, self.pause),
                )?;
            }
            "variables" => {
                self.require_pause(&call.arguments)?;
                self.send(
                    "scopes",
                    json!({"frameId":call.arguments["frame"]}),
                    Pending::Scopes(reply, self.pause),
                )?;
            }
            "set_breakpoints" => {
                let mut groups = group_breakpoints(&call.arguments)?;
                // Empty groups explicitly remove every previously bound source; retained set lives in launch.
                for (source, _) in group_breakpoints(&self.launch)? {
                    if !groups.iter().any(|(path, _)| path == &source) {
                        groups.push_back((source, vec![]));
                    }
                }
                self.launch["breakpoints"] = call.arguments["breakpoints"].clone();
                if let Some((source, lines)) = groups.pop_front() {
                    self.send(
                        "setBreakpoints",
                        breakpoint_args(&source, &lines),
                        Pending::Breakpoints {
                            reply,
                            groups,
                            values: vec![],
                            source,
                        },
                    )?;
                } else {
                    return Ok(Some(json!({"breakpoints":[]})));
                }
            }
            _ => {
                return Err(Failure::new(
                    ErrorCode::UnsupportedOperation,
                    "Unknown debug method",
                ));
            }
        }
        Ok(None)
    }
    /// A cancelled public wait releases only its correlation; cancellation never replays a launch.
    pub fn cancel_reply(&mut self, request: &ResourceHandle) {
        if self.start_reply.as_ref() == Some(request) {
            self.start_reply = None;
        }
        // Cancelling a wait cannot discard the correlation that will finish an already-sent motion
        // or disconnect. Keep that protocol state and remove only the public reply authority.
        for pending in self.pending.values_mut() {
            match pending {
                Pending::Control { reply, .. } | Pending::Stop(reply)
                    if reply.as_ref() == Some(request) =>
                {
                    *reply = None
                }
                _ => {}
            }
        }
        self.pending
            .retain(|_, pending| pending.reply() != Some(request));
        self.stop_replies.retain(|reply| reply != request);
    }
    /// Release guest authority; the host retains the native observer through asynchronous tree/EOF cleanup.
    pub fn release(&mut self) {
        if let Some(handle) = self.terminal.take() {
            let _ = api::guest::request(api::Operation::CloseResource { handle });
        }
        let _ = api::guest::request(api::Operation::CloseResource {
            handle: self.handle.clone(),
        });
        self.state = "exited";
    }
    /// stdout carries framed messages, stderr carries diagnostics, and terminal events settle waits.
    pub fn update(&mut self, update: process::Update) {
        let result = match update {
            process::Update::Output {
                stream: process::Stream::Stdout,
                bytes,
            } => self.transport.receive(&bytes).and_then(|messages| {
                for message in messages {
                    self.message(message)?;
                }
                Ok(())
            }),
            process::Update::Output { bytes, .. } => {
                append_bounded(&mut self.diagnostics, &String::from_utf8_lossy(&bytes));
                self.publish_output(&String::from_utf8_lossy(&bytes))
            }
            process::Update::Exited { .. } | process::Update::Terminated => {
                self.finish_exit();
                Ok(())
            }
        };
        if let Err(error) = result {
            append_bounded(&mut self.diagnostics, &error.message);
            let _ = self.publish_output(&error.message);
            self.fail_pending(error);
            self.release();
        }
    }
    /// Start completes only after launch and configuration acknowledgements, in either response order.
    fn maybe_started(&mut self) {
        if self.configured && self.launched {
            if self.state == "starting" {
                self.state = "running";
            }
            if let Some(reply) = self.start_reply.take() {
                answer(reply, Ok(self.receipt()));
            }
        }
    }
    /// Group breakpoints by source because DAP replaces the entire set of each source.
    fn configure_next(&mut self) -> Result<(), Failure> {
        if let Some((source, lines)) = self.breakpoints.pop_front() {
            self.send(
                "setBreakpoints",
                breakpoint_args(&source, &lines),
                Pending::InitialBreakpoints,
            )?;
        } else {
            self.send("configurationDone", json!({}), Pending::Configured)?;
        }
        Ok(())
    }
    /// Only decoded DAP text and adapter diagnostics enter the terminal; framed stdout stays private.
    fn publish_output(&self, text: &str) -> Result<(), Failure> {
        let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
        for bytes in normalized.as_bytes().chunks(16 * 1024) {
            api::guest::request(api::Operation::Process {
                operation: process::Operation::TerminalOutput {
                    handle: self.handle.clone(),
                    title: self.launch["name"].as_str().unwrap_or("Debug").into(),
                    bytes: bytes.to_vec(),
                },
            })?;
        }
        Ok(())
    }
    fn send(&mut self, command: &str, arguments: Value, pending: Pending) -> Result<(), Failure> {
        if self.pending.len() >= 32 {
            return Err(Failure::new(
                ErrorCode::LimitExceeded,
                "DAP correlation quota exceeded",
            ));
        }
        let sequence = self.transport.send(&self.handle, command, arguments)?;
        self.pending.insert(sequence, pending);
        Ok(())
    }
    fn receipt(&self) -> Value {
        json!({"session":self.id,"state":self.state,"pause":self.pause})
    }
    fn status(&self) -> Value {
        let mut value = self.receipt();
        if self.state == "paused" {
            value["reason"] = json!(self.reason);
            if let Some(source) = &self.source {
                value["source"] = json!(source);
            }
            if let Some(line) = self.line {
                value["line"] = json!(line);
            }
        }
        value
    }
    /// Epochs bind public frame IDs to the pause the caller actually observed, including delayed calls.
    fn require_pause(&self, arguments: &Value) -> Result<(), Failure> {
        if self.state != "paused" || self.moving.is_some() {
            return Err(Failure::new(
                ErrorCode::InvalidState,
                "Target has no inspectable pause",
            ));
        }
        if arguments["pause"].as_u64() != Some(self.pause) {
            return Err(Failure::new(
                ErrorCode::InvalidState,
                "Pause reference expired",
            ));
        }
        Ok(())
    }
    fn invalidate_pause(&mut self) -> Result<(), Failure> {
        self.inspection.clear();
        self.pause = self
            .pause
            .checked_add(1)
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or_else(|| {
                Failure::new(ErrorCode::LimitExceeded, "Debug pause identity exhausted")
            })?;
        self.source = None;
        self.line = None;
        Ok(())
    }
    fn valid_pause(&self, pause: u64, reply: &ResourceHandle) -> bool {
        if self.state == "paused" && self.moving.is_none() && self.pause == pause {
            true
        } else {
            answer(
                reply.clone(),
                Err(Failure::new(
                    ErrorCode::InvalidState,
                    "Pause reference expired",
                )),
            );
            false
        }
    }
    fn finish_exit(&mut self) {
        self.state = "exited";
        self.moving = None;
        let _ = self.invalidate_pause();
        let mut stops = std::mem::take(&mut self.stop_replies);
        // Adapter shutdown may precede its disconnect response, but actual tree completion is enough.
        for pending in self.pending.values() {
            if let Pending::Stop(Some(reply)) = pending {
                stops.push(reply.clone());
            }
        }
        for reply in stops {
            answer(reply, Ok(self.receipt()));
        }
        self.pending
            .retain(|_, pending| !matches!(pending, Pending::Stop(_)));
        self.fail_pending(failure(format!(
            "Java debug adapter exited: {}",
            bounded(&self.diagnostics, 2048)
        )));
    }
    fn fail_pending(&mut self, error: Failure) {
        if let Some(reply) = self.start_reply.take() {
            answer(reply, Err(error.clone()));
        }
        for (_, pending) in std::mem::take(&mut self.pending) {
            if let Some(reply) = pending.reply().cloned() {
                answer(reply, Err(error.clone()));
            }
        }
    }
}
impl Pending {
    /// This is correlation metadata only; resource ownership remains with the original source.
    fn reply(&self) -> Option<&ResourceHandle> {
        match self {
            Self::Inspection(request) => Some(request.reply()),
            Self::Control { reply, .. } | Self::Stop(reply) => reply.as_ref(),
            Self::Frames(reply, _)
            | Self::Scopes(reply, _)
            | Self::Variables { reply, .. }
            | Self::Breakpoints { reply, .. } => Some(reply),
            _ => None,
        }
    }
}
/// Apply the service byte quota before replying. A refused reply retains its handle so an explicit
/// bounded failure can finish the caller instead of silently converting valid work into a timeout.
fn answer(reply: ResourceHandle, value: Result<Value, Failure>) {
    let value = bounded_reply(value);
    if let Err(error) = service::guest::reply(&reply, value) {
        if error.code != ErrorCode::InvalidHandle {
            let failure = Failure::new(error.code, bounded(&error.message, 512));
            if service::guest::reply(&reply, Err(failure)).is_err() {
                let _ = api::guest::request(api::Operation::CloseResource { handle: reply });
            }
        }
    }
}
/// Measure the final encoded value, since item counts alone do not bound strings or escaping.
fn bounded_reply(value: Result<Value, Failure>) -> Result<Value, Failure> {
    if let Ok(value) = &value {
        if serde_json::to_vec(value).map_or(true, |bytes| bytes.len() > 60 * 1024) {
            return Err(Failure::new(
                ErrorCode::LimitExceeded,
                "Debug result exceeds the service byte budget",
            ));
        }
    }
    value
}
/// UTF-8 clipping never cuts a scalar and keeps both native UI and per-session memory bounded.
fn bounded(text: &str, maximum: usize) -> String {
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
fn append_bounded(target: &mut String, text: &str) {
    target.push_str(text);
    if target.len() > 8192 {
        let mut start = target.len() - 8192;
        while !target.is_char_boundary(start) {
            start += 1;
        }
        target.drain(..start);
    }
}
fn group_breakpoints(arguments: &Value) -> Result<VecDeque<(String, Vec<u32>)>, Failure> {
    let mut groups: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for breakpoint in arguments["breakpoints"].as_array().into_iter().flatten() {
        let source = breakpoint["source"]
            .as_str()
            .ok_or_else(|| failure("Missing breakpoint source"))?;
        let line = breakpoint["line"]
            .as_u64()
            .and_then(|line| u32::try_from(line).ok())
            .filter(|line| *line != 0)
            .ok_or_else(|| failure("Invalid breakpoint line"))?;
        groups.entry(source.into()).or_default().push(line);
    }
    Ok(groups.into_iter().collect())
}
fn breakpoint_args(source: &str, lines: &[u32]) -> Value {
    json!({"source":{"path":source},"breakpoints":lines.iter().map(|line| json!({"line":line})).collect::<Vec<_>>()})
}
