//! Java-owned DAP policy adapted from Nanobug Rust; MIT notice is in licenses/nanobug-rust-LICENSE.txt.
use super::*;

impl Session {
    pub(super) fn message(&mut self, message: Value) -> Result<(), Failure> {
        if message["type"] == "event" {
            return self.event(&message);
        }
        if message["type"] == "request" {
            let result = self.reverse_terminal(&message);
            self.transport.response(
                &self.handle,
                &message,
                result
                    .as_ref()
                    .map(|_| json!({}))
                    .map_err(|error| error.message.clone()),
            )?;
            return result;
        }
        let Some(sequence) = message["request_seq"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
        else {
            return Err(failure("DAP response has no correlation"));
        };
        let Some(pending) = self.pending.remove(&sequence) else {
            return Ok(());
        };
        if message["success"] != true {
            let error = failure(
                message["message"]
                    .as_str()
                    .unwrap_or("Adapter rejected the operation"),
            );
            if let Pending::Control { moves, pause, .. } = &pending {
                if *moves && self.moving == Some(*pause) {
                    self.moving = None;
                }
                if !moves {
                    self.pausing = None;
                }
            }
            if let Some(reply) = pending.reply().cloned() {
                answer(reply, Err(error));
                return Ok(());
            }
            if matches!(pending, Pending::Control { .. } | Pending::Stop(_)) {
                return Ok(());
            }
            return Err(error);
        }
        let body = &message["body"];
        match pending {
            Pending::Freeze => {}
            Pending::Inspection(request) => {
                let reply = request.reply().clone();
                if let Err(error) = self.inspected(request, body) {
                    answer(reply, Err(error));
                }
            }
            Pending::Initialize => {
                let args = self.launch["dap"].clone();
                self.send("launch", args, Pending::Launch)?;
            }
            Pending::Launch => {
                self.launched = true;
                self.maybe_started();
            }
            Pending::InitialBreakpoints => self.configure_next()?,
            Pending::Configured => {
                self.configured = true;
                self.maybe_started();
            }
            Pending::Control {
                reply,
                moves,
                pause,
            } => {
                if moves && self.moving == Some(pause) {
                    self.moving = None;
                    // A stopped event may have overtaken this response; preserve that newer pause.
                    if self.pause == pause && self.state == "paused" {
                        self.invalidate_pause()?;
                        self.state = "running";
                    }
                }
                if let Some(reply) = reply {
                    answer(reply, Ok(self.receipt()));
                }
            }
            Pending::Stop(reply) => {
                // DAP acknowledges termination intent. Only actual native job completion ends stop.
                if let Some(reply) = reply {
                    self.stop_replies.push(reply);
                }
                api::guest::request(api::Operation::Process {
                    operation: process::Operation::RequestExit {
                        handle: self.handle.clone(),
                        mode: process::ExitMode::Force,
                    },
                })?;
            }
            Pending::Location(pause) => {
                if pause == self.pause && self.state == "paused" {
                    // Runtime frames may lack source. Retain the first real source-bearing frame
                    // rather than inventing a path or position for native implementation details.
                    if let Some(frame) = body["stackFrames"].as_array().and_then(|frames| {
                        frames
                            .iter()
                            .find(|frame| frame["source"]["path"].as_str().is_some())
                    }) {
                        self.source = frame["source"]["path"].as_str().map(str::to_owned);
                        self.line = frame["line"]
                            .as_u64()
                            .and_then(|line| u32::try_from(line).ok());
                    }
                }
            }
            Pending::Frames(reply, pause) => {
                if self.valid_pause(pause, &reply) {
                    let frames = body["stackFrames"].as_array().ok_or_else(|| failure("Missing DAP stack frames"))?.iter().take(256)
                        .filter_map(|frame| Some(json!({"id":frame["id"],"name":bounded(frame["name"].as_str()?,512),"source":frame["source"]["path"].as_str()?,"line":frame["line"].as_u64()?.max(1)}))).collect::<Vec<_>>();
                    answer(reply, Ok(json!({"frames":frames})));
                }
            }
            Pending::Scopes(reply, pause) => {
                if self.valid_pause(pause, &reply) {
                    let mut references: VecDeque<_> = body["scopes"]
                        .as_array()
                        .ok_or_else(|| failure("Missing DAP scopes"))?
                        .iter()
                        .take(16)
                        .filter(|scope| scope["expensive"] != true)
                        .filter_map(|scope| {
                            scope["variablesReference"].as_u64().filter(|id| *id != 0)
                        })
                        .collect();
                    if let Some(reference) = references.pop_front() {
                        self.send(
                            "variables",
                            json!({"variablesReference":reference,"count":512}),
                            Pending::Variables {
                                reply,
                                pause,
                                references,
                                values: vec![],
                            },
                        )?;
                    } else {
                        answer(reply, Ok(json!({"variables":[]})));
                    }
                }
            }
            Pending::Variables {
                reply,
                pause,
                mut references,
                mut values,
            } => {
                if self.valid_pause(pause, &reply) {
                    for variable in body["variables"]
                        .as_array()
                        .ok_or_else(|| failure("Missing DAP variables"))?
                    {
                        if values.len() == 512 {
                            break;
                        }
                        values.push(json!({"name":bounded(variable["name"].as_str().unwrap_or_default(),512),"value":bounded(variable["value"].as_str().unwrap_or_default(),4096)}));
                    }
                    if let Some(reference) = references.pop_front() {
                        self.send(
                            "variables",
                            json!({"variablesReference":reference,"count":512-values.len()}),
                            Pending::Variables {
                                reply,
                                pause,
                                references,
                                values,
                            },
                        )?;
                    } else {
                        answer(reply, Ok(json!({"variables":values})));
                    }
                }
            }
            Pending::Breakpoints {
                reply,
                mut groups,
                mut values,
                source,
            } => {
                for breakpoint in body["breakpoints"]
                    .as_array()
                    .ok_or_else(|| failure("Missing breakpoint acknowledgements"))?
                {
                    values.push(json!({"source":source,"line":breakpoint["line"].as_u64().unwrap_or(1).max(1),"verified":breakpoint["verified"] == true}));
                }
                if let Some((source, lines)) = groups.pop_front() {
                    self.send(
                        "setBreakpoints",
                        breakpoint_args(&source, &lines),
                        Pending::Breakpoints {
                            reply,
                            groups,
                            values,
                            source,
                        },
                    )?;
                } else {
                    answer(reply, Ok(json!({"breakpoints":values})));
                }
            }
        }
        Ok(())
    }
    /// Adapter events update only this target; pause state is never inferred from elapsed time.
    fn event(&mut self, message: &Value) -> Result<(), Failure> {
        let body = &message["body"];
        match message["event"].as_str().unwrap_or_default() {
            "initialized" => self.configure_next()?,
            "thread" => {
                if body["reason"] == "started" && self.thread == 0 {
                    self.thread = body["threadId"].as_u64().unwrap_or(self.thread);
                }
            }
            "stopped" => {
                if body["allThreadsStopped"] != true {
                    if self.freezing.is_none() {
                        let thread = body["threadId"].as_u64().unwrap_or(self.thread);
                        self.pausing = Some(thread);
                        self.freezing =
                            Some(bounded(body["reason"].as_str().unwrap_or("paused"), 64));
                        self.send("pauseAll", json!({"threadId":thread}), Pending::Freeze)?;
                    }
                    return Ok(());
                }
                if let Some(description) = body["description"].as_str() {
                    append_bounded(&mut self.diagnostics, description);
                }
                self.invalidate_pause()?;
                self.state = "paused";
                self.moving = None;
                let requested = self
                    .pausing
                    .take()
                    .filter(|_| body["allThreadsStopped"] == true);
                self.thread = requested
                    .or_else(|| body["threadId"].as_u64())
                    .unwrap_or(self.thread);
                self.reason = self
                    .freezing
                    .take()
                    .unwrap_or_else(|| bounded(body["reason"].as_str().unwrap_or("paused"), 64));
                self.send(
                    "stackTrace",
                    json!({"threadId":self.thread,"startFrame":0,"levels":256}),
                    Pending::Location(self.pause),
                )?;
            }
            "continued" => {
                self.invalidate_pause()?;
                self.state = "running";
                self.moving = None;
            }
            "output" => self.publish_output(body["output"].as_str().unwrap_or_default())?,
            "terminated" => {
                // The target is gone. Release the enclosing adapter job and complete outstanding stops.
                self.release();
                self.finish_exit();
            }
            _ => {}
        }
        Ok(())
    }
    /// Only the adapter requests its terminal agent. Java DAP still launches exactly one debug target.
    fn reverse_terminal(&mut self, message: &Value) -> Result<(), Failure> {
        if message["command"] != "runInTerminal" || self.terminal.is_some() {
            return Err(failure("Unsupported or duplicate adapter terminal request"));
        }
        let args = &message["arguments"];
        if args["kind"] == "external" || args["argsCanBeInterpretedByShell"] == true {
            return Err(failure(
                "Only literal integrated terminal requests are supported",
            ));
        }
        let argv = args["args"]
            .as_array()
            .ok_or_else(|| failure("Missing terminal argv"))?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| failure("Invalid terminal argv"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let program = argv
            .first()
            .cloned()
            .ok_or_else(|| failure("Empty terminal argv"))?;
        // The owned UTF-8 launcher creates exactly the JVM requested by this trusted adapter.
        let mut arguments = vec!["run".into(), "--java".into(), program, "--".into()];
        arguments.extend(argv.into_iter().skip(1));
        let program = self.launch["program"]
            .as_str()
            .ok_or_else(|| failure("Missing Java launcher"))?
            .to_owned();
        let env = args["env"]
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(name, value)| {
                        value
                            .as_str()
                            .map(|value| (name.clone(), value.into()))
                            .ok_or_else(|| failure("Terminal environment removal is unsupported"))
                    })
                    .collect::<Result<BTreeMap<_, _>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let cwd = args["cwd"]
            .as_str()
            .filter(|cwd| !cwd.is_empty())
            .or_else(|| self.launch["cwd"].as_str())
            .map(str::to_owned);
        let api::Value::Resource(handle) = api::guest::request(api::Operation::Process {
            operation: process::Operation::Execute {
                program,
                args: arguments,
                cwd,
                env,
                transport: process::Transport::Pty {
                    columns: 80,
                    rows: 24,
                    inherit_cursor: false,
                },
            },
        })?
        else {
            return Err(failure("Expected terminal process resource"));
        };
        self.terminal = Some(handle.clone());
        api::guest::request(api::Operation::Process {
            operation: process::Operation::PresentTerminal {
                handle,
                title: self.launch["name"].as_str().unwrap_or("Debug").into(),
            },
        })?;
        Ok(())
    }
    /// Losing the terminal agent is a real I/O failure; never silently continue a detached target.
    pub fn terminal_update(&mut self, handle: &ResourceHandle, update: &process::Update) {
        if self.terminal.as_ref() == Some(handle)
            && matches!(
                update,
                process::Update::Exited { .. } | process::Update::Terminated
            )
            && !self.ended()
        {
            self.fail_pending(failure("Debug terminal exited"));
            self.release();
            self.finish_exit();
        }
    }
}
