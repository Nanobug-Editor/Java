//! Thread/frame proofs and lazy variable references are scoped to one observed pause, never reused.
use super::*;
use std::collections::BTreeSet;

/// Native DAP references stay private; public references also bind the owning thread and frame.
#[derive(Default)]
pub(super) struct Inspection {
    threads: BTreeSet<u64>,
    frames: BTreeMap<u64, BTreeSet<u64>>,
    references: BTreeMap<u32, Reference>,
    next: u32,
}
struct Reference {
    thread: u64,
    frame: u64,
    backend: u64,
    paged: bool,
    lazy: bool,
}
impl Inspection {
    /// Ended pauses revoke all handles, while the counter prevents accidental identity recycling.
    pub(super) fn clear(&mut self) {
        self.threads.clear();
        self.frames.clear();
        self.references.clear();
    }
    fn reference(
        &mut self,
        thread: u64,
        frame: u64,
        backend: u64,
        paged: bool,
        lazy: bool,
    ) -> Result<u32, Failure> {
        if backend == 0 {
            return Ok(0);
        }
        if let Some((id, _)) = self.references.iter().find(|(_, value)| {
            value.thread == thread && value.frame == frame && value.backend == backend
        }) {
            return Ok(*id);
        }
        if self.references.len() >= 4096 {
            return Err(failure("Variable reference quota exceeded"));
        }
        self.next = self
            .next
            .checked_add(1)
            .filter(|id| *id <= i32::MAX as u32)
            .ok_or_else(|| failure("Variable identity exhausted"))?;
        self.references.insert(
            self.next,
            Reference {
                thread,
                frame,
                backend,
                paged,
                lazy,
            },
        );
        Ok(self.next)
    }
}

/// Deferred requests retain all four inspection identities until a response is actually admitted.
pub(super) enum Request {
    Resolve {
        page: Page,
        reference: u32,
    },
    Threads {
        reply: ResourceHandle,
        pause: u64,
    },
    Frames {
        reply: ResourceHandle,
        pause: u64,
        thread: u64,
    },
    Scopes(Page),
    Variables {
        page: Page,
        roots: Option<VecDeque<u64>>,
        skip: usize,
        values: Vec<Value>,
    },
}
pub(super) struct Page {
    reply: ResourceHandle,
    pause: u64,
    thread: u64,
    frame: u64,
    start: usize,
    count: usize,
}
impl Request {
    pub(super) fn reply(&self) -> &ResourceHandle {
        match self {
            Self::Threads { reply, .. } | Self::Frames { reply, .. } => reply,
            Self::Scopes(page) | Self::Variables { page, .. } | Self::Resolve { page, .. } => {
                &page.reply
            }
        }
    }
}

impl Session {
    /// Reject unobserved threads/frames and foreign references before asking the native adapter.
    pub(super) fn inspect(&mut self, call: service::Invocation) -> Result<(), Failure> {
        self.require_pause(&call.arguments)?;
        let reply = call
            .reply
            .ok_or_else(|| failure("Missing inspection reply"))?;
        if call.method == "threads" {
            return self.send(
                "threads",
                json!({}),
                Pending::Inspection(Request::Threads {
                    reply,
                    pause: self.pause,
                }),
            );
        }
        let thread = call.arguments["thread"]
            .as_u64()
            .ok_or_else(|| failure("Missing thread"))?;
        if !self.inspection.threads.contains(&thread) {
            return Err(failure("Thread is not part of this pause"));
        }
        if call.method == "thread_frames" {
            return self.send(
                "stackTrace",
                json!({"threadId":thread,"startFrame":0,"levels":256}),
                Pending::Inspection(Request::Frames {
                    reply,
                    pause: self.pause,
                    thread,
                }),
            );
        }
        let frame = call.arguments["frame"]
            .as_u64()
            .ok_or_else(|| failure("Missing frame"))?;
        if !self
            .inspection
            .frames
            .get(&thread)
            .is_some_and(|frames| frames.contains(&frame))
        {
            return Err(failure("Frame is not part of this thread and pause"));
        }
        let page = Page {
            reply,
            pause: self.pause,
            thread,
            frame,
            start: call.arguments["start"].as_u64().unwrap_or(0) as usize,
            count: call.arguments["count"]
                .as_u64()
                .unwrap_or(100)
                .clamp(1, 100) as usize,
        };
        let reference = call.arguments["reference"].as_u64().unwrap_or(0);
        if reference == 0 {
            return self.send(
                "scopes",
                json!({"frameId":frame}),
                Pending::Inspection(Request::Scopes(page)),
            );
        }
        let value = u32::try_from(reference)
            .ok()
            .and_then(|id| self.inspection.references.get(&id))
            .filter(|value| value.thread == thread && value.frame == frame)
            .ok_or_else(|| {
                failure("Variable reference belongs to a different pause, thread or frame")
            })?;
        if value.lazy {
            // Java first resolves a lazy presentation into a new container. Do this only on
            // expansion; its single presentation value is not the requested child page.
            return self.send(
                "variables",
                json!({"variablesReference":value.backend}),
                Pending::Inspection(Request::Resolve {
                    page,
                    reference: reference as u32,
                }),
            );
        }
        // Java's adapter accepts native paging only for array-backed containers. Ordinary
        // objects expose a bounded field reply, sliced here without eagerly expanding children.
        let args = if value.paged {
            json!({"variablesReference":value.backend,"start":page.start,"count":page.count+1})
        } else {
            json!({"variablesReference":value.backend})
        };
        let skip = if value.paged { 0 } else { page.start };
        self.send(
            "variables",
            args,
            Pending::Inspection(Request::Variables {
                page,
                roots: None,
                skip,
                values: vec![],
            }),
        )
    }

    /// Responses are inert unless the exact pause still exists; thread switches do not retarget them.
    pub(super) fn inspected(&mut self, request: Request, body: &Value) -> Result<(), Failure> {
        match request {
            Request::Resolve { page, reference } => {
                if !self.valid_pause(page.pause, &page.reply) {
                    return Ok(());
                }
                let resolved = body["variables"]
                    .as_array()
                    .filter(|values| values.len() == 1)
                    .and_then(|values| values.first())
                    .ok_or_else(|| failure("Invalid lazy variable resolution"))?;
                let backend = resolved["variablesReference"].as_u64().unwrap_or(0);
                if backend == 0 {
                    answer(page.reply, Ok(json!({"variables":[],"more":false})));
                    return Ok(());
                }
                let value = self
                    .inspection
                    .references
                    .get_mut(&reference)
                    .ok_or_else(|| failure("Lazy reference expired"))?;
                value.backend = backend;
                value.paged = resolved["indexedVariables"].as_u64().unwrap_or(0) > 0;
                value.lazy = false;
                let args = if value.paged {
                    json!({"variablesReference":backend,"start":page.start,"count":page.count+1})
                } else {
                    json!({"variablesReference":backend})
                };
                let skip = if value.paged { 0 } else { page.start };
                self.send(
                    "variables",
                    args,
                    Pending::Inspection(Request::Variables {
                        page,
                        roots: None,
                        skip,
                        values: vec![],
                    }),
                )?;
            }
            Request::Threads { reply, pause } => {
                if !self.valid_pause(pause, &reply) {
                    return Ok(());
                }
                let raw = body["threads"]
                    .as_array()
                    .ok_or_else(|| failure("Missing adapter threads"))?;
                if raw.len() > 256 {
                    answer(
                        reply,
                        Err(failure("Thread list exceeds the contract bound")),
                    );
                    return Ok(());
                }
                let threads:Vec<_> = raw.iter().filter_map(|thread| {
                    let id=thread["id"].as_u64().filter(|id| *id>0 && *id<=i32::MAX as u64)?;
                    Some(json!({"id":id,"name":bounded(thread["name"].as_str().unwrap_or(""),512)}))
                }).collect();
                self.inspection.threads = threads
                    .iter()
                    .filter_map(|thread| thread["id"].as_u64())
                    .collect();
                answer(reply, Ok(json!({"threads":threads,"selected":self.thread})));
            }
            Request::Frames {
                reply,
                pause,
                thread,
            } => {
                if !self.valid_pause(pause, &reply) {
                    return Ok(());
                }
                let frames:Vec<_> = body["stackFrames"].as_array().ok_or_else(|| failure("Missing adapter frames"))?.iter().take(256).filter_map(|frame| {
                    Some(json!({"id":frame["id"].as_u64().filter(|id| *id<=i32::MAX as u64)?,"name":bounded(frame["name"].as_str()?,512),"source":frame["source"]["path"].as_str()?,"line":frame["line"].as_u64()?.max(1)}))
                }).collect();
                self.inspection.frames.insert(
                    thread,
                    frames
                        .iter()
                        .filter_map(|frame| frame["id"].as_u64())
                        .collect(),
                );
                answer(reply, Ok(json!({"frames":frames})));
            }
            Request::Scopes(page) => {
                if !self.valid_pause(page.pause, &page.reply) {
                    return Ok(());
                }
                let mut roots: VecDeque<_> = body["scopes"]
                    .as_array()
                    .ok_or_else(|| failure("Missing adapter scopes"))?
                    .iter()
                    .take(16)
                    .filter(|scope| scope["expensive"] != true)
                    .filter_map(|scope| scope["variablesReference"].as_u64().filter(|id| *id != 0))
                    .collect();
                if let Some(root) = roots.pop_front() {
                    let skip = page.start;
                    self.send(
                        "variables",
                        json!({"variablesReference":root}),
                        Pending::Inspection(Request::Variables {
                            page,
                            roots: Some(roots),
                            skip,
                            values: vec![],
                        }),
                    )?;
                } else {
                    answer(page.reply, Ok(json!({"variables":[],"more":false})));
                }
            }
            Request::Variables {
                page,
                mut roots,
                mut skip,
                mut values,
            } => {
                if !self.valid_pause(page.pause, &page.reply) {
                    return Ok(());
                }
                let raw = body["variables"]
                    .as_array()
                    .ok_or_else(|| failure("Missing adapter variables"))?;
                for variable in raw {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    if values.len() > page.count {
                        break;
                    }
                    let reference = self.inspection.reference(
                        page.thread,
                        page.frame,
                        variable["variablesReference"].as_u64().unwrap_or(0),
                        variable["indexedVariables"].as_u64().unwrap_or(0) > 0,
                        variable["presentationHint"]["lazy"] == true,
                    )?;
                    values.push(json!({"name":bounded(variable["name"].as_str().unwrap_or(""),512),"value":bounded(variable["value"].as_str().unwrap_or(""),4096),"reference":reference,
                        "named":variable["namedVariables"].as_u64().unwrap_or(0).min(i32::MAX as u64),"indexed":variable["indexedVariables"].as_u64().unwrap_or(0).min(i32::MAX as u64)}));
                }
                if values.len() <= page.count
                    && let Some(root) = roots.as_mut().and_then(VecDeque::pop_front)
                {
                    return self.send(
                        "variables",
                        json!({"variablesReference":root}),
                        Pending::Inspection(Request::Variables {
                            page,
                            roots,
                            skip,
                            values,
                        }),
                    );
                }
                let mut more = values.len() > page.count
                    || roots.as_ref().is_some_and(|roots| !roots.is_empty());
                values.truncate(page.count);
                // Bound encoded bytes as well as item count. Return a shorter genuine page and
                // let its actual length advance the next request; never skip an oversized suffix.
                let mut budget = 58 * 1024;
                let mut kept = 0;
                for value in &values {
                    let bytes = serde_json::to_vec(value)
                        .map_err(|error| failure(error.to_string()))?
                        .len()
                        + 1;
                    if bytes > budget {
                        more = true;
                        break;
                    }
                    budget -= bytes;
                    kept += 1;
                }
                values.truncate(kept);
                answer(page.reply, Ok(json!({"variables":values,"more":more})));
            }
        }
        Ok(())
    }
}
