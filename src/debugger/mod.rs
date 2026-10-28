//! Java-owned DAP policy adapted from Nanobug Rust; MIT notice is in licenses/nanobug-rust-LICENSE.txt.
mod launch;
mod session;
mod transport;
use plugin_protocol::{
    api::{self, ErrorCode, Failure},
    service,
};
use session::Session;
use std::{cell::RefCell, collections::BTreeMap};

#[derive(Default)]
struct Debugger {
    sessions: BTreeMap<String, Session>,
    next: u64,
}
thread_local! { static STATE:RefCell<Debugger> = RefCell::new(Debugger::default()); }
/// Dispatch debugger work independently from compiler preparation and language analysis.
pub(crate) fn event(input: api::Notification) -> Result<api::Output, Failure> {
    STATE.with(|state| state.borrow_mut().dispatch(input))
}

impl Debugger {
    /// Route only exact source-owned session IDs. Installation and preparation never launch adapters.
    fn dispatch(&mut self, input: api::Notification) -> Result<api::Output, Failure> {
        let mut reply = None;
        match input {
            api::Notification::Service(service::Notification::Invoke(call)) => {
                let result = if call.method == "start" {
                    self.start(call)
                } else {
                    let id = call.arguments["session"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    self.sessions
                        .get_mut(&id)
                        .filter(|session| session.owner == call.caller.instance)
                        .ok_or_else(|| {
                            Failure::new(
                                ErrorCode::InvalidHandle,
                                "Unknown or foreign debug session",
                            )
                        })
                        .and_then(|session| session.call(call))
                };
                reply = match result {
                    Ok(value) => value.map(Ok),
                    Err(error) => Some(Err(error)),
                };
            }
            api::Notification::Process { handle, update } => {
                for session in self.sessions.values_mut() {
                    if session.handle == handle {
                        session.update(update.clone());
                    } else {
                        session.terminal_update(&handle, &update);
                    }
                }
            }
            api::Notification::Service(service::Notification::InvocationCancelled {
                request,
                ..
            }) => {
                // Cancelling a receipt stops that wait. A created target remains owned and stoppable.
                for session in self.sessions.values_mut() {
                    session.cancel_reply(&request);
                }
            }
            _ => {}
        }
        // Presentation uses the public process API; this provider contributes no separate panel.
        Ok(api::Output {
            service_reply: reply,
            // Logical snapshot excludes native handles and targets; recovery must never replay them.
            snapshot: Some(plugin_protocol::Snapshot {
                schema: 1,
                data: "{}".into(),
            }),
            ..Default::default()
        })
    }
    /// A target owns one bridge/adapter job. Finished histories may be evicted; active jobs may not.
    fn start(&mut self, call: service::Invocation) -> Result<Option<serde_json::Value>, Failure> {
        while self.sessions.len() >= 32 {
            let Some(id) = self
                .sessions
                .iter()
                .find(|(_, session)| session.ended())
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            self.sessions.remove(&id);
        }
        if self.sessions.len() >= 32 {
            return Err(Failure::new(
                ErrorCode::LimitExceeded,
                "Debug session quota exceeded",
            ));
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| transport::failure("Debug identity exhausted"))?;
        let id = self.next.to_string();
        let session = Session::start(id.clone(), call)?;
        self.sessions.insert(id, session);
        Ok(None)
    }
}
