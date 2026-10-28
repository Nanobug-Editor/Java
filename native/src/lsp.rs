//! The Java adapter exposes request-bound diagnostics over standard LSP and retains only source hashes.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader, Read, Write},
    process::{ChildStdin, ChildStdout},
    sync::{Arc, Mutex},
};

/// Metadata correlates one initialization and at most 32 in-flight diagnostics; no document text is kept.
#[derive(Default)]
struct State {
    /// Host request ID used to augment the real backend's initialization capabilities.
    initialize: Option<Value>,
    /// Exact full-sync source hashes, removed on document close.
    sources: BTreeMap<String, String>,
    /// Each pending request retains its own expected hash, independently of later source updates.
    diagnostics: BTreeMap<String, String>,
    /// The host's actual resource offer remains authoritative despite JDT's all-or-nothing gate.
    resource_operations: BTreeSet<String>,
    /// Bounded request identities let complete rename replies be checked before forwarding.
    renames: BTreeSet<String>,
}

/// Relay the owned backend's `input`/`output`; framing stays standard and request IDs stay unchanged.
/// The main reader exits with the backend. The host's native process job owns both process lifetimes.
pub(crate) fn relay(
    mut input: ChildStdin,
    output: ChildStdout,
    mut source: BufReader<std::io::Stdin>,
    first: Value,
) -> Result<()> {
    let state = Arc::new(Mutex::new(State::default()));
    let client = Arc::new(Mutex::new(std::io::stdout()));
    let incoming_state = state.clone();
    let incoming_client = client.clone();
    std::thread::Builder::new().name("java-lsp-input".into()).spawn(move || {
        let result = (|| -> Result<()> {
            let mut next = Some(first);
            loop {
                let Some(mut message) = (match next.take() { Some(message) => Some(message), None => read_frame(&mut source)? }) else { break; };
                let mut state = incoming_state.lock().unwrap();
                // The owned bundle establishes the explicitly supplied generic project before ACK.
                // It never discovers or evaluates a build tool's project format.
                if message["method"]=="java/project/ready" {
                    message["method"]=json!("workspace/executeCommand");
                    message["params"]=json!({"command":"nanobug.java.project.ready","arguments":[]});
                }
                if message["method"]=="nanobug/controlConsumersRetired" {continue;}
                if let Err(reason) = state.incoming(&mut message) {
                    if let Some(id) = message.get("id") {
                        write_frame(&mut *incoming_client.lock().unwrap(), &json!({
                            "jsonrpc":"2.0", "id":id, "error":{"code":-32801,"message":reason}
                        }))?;
                    }
                    continue;
                }
                drop(state);
                write_frame(&mut input, &message)?;
            }
            Ok(())
        })();
        if let Err(error) = result { eprintln!("Java LSP input: {error:#}"); }
    })?;
    let mut output = BufReader::new(output);
    while let Some(mut message) = read_frame(&mut output)? {
        // JDT reuses language/status for Ready, progress and project messages. A separate startup
        // signal cannot be reset by a later unrelated status while native source control waits.
        let ready =
            message["method"] == "language/status" && message["params"]["type"] == "Started";
        state.lock().unwrap().outgoing(&mut message);
        let mut client = client.lock().unwrap();
        write_frame(&mut *client, &message)?;
        if ready {
            write_frame(
                &mut *client,
                &json!({"jsonrpc":"2.0","method":"nanobug/javaReady","params":{"ready":true}}),
            )?;
        }
    }
    Ok(())
}

impl State {
    /// Translate one full-source diagnostic request into the installed JDT bundle's pure query.
    /// Notifications update hashes only; incremental edits cannot acquire a false source proof.
    fn incoming(&mut self, message: &mut Value) -> Result<(), &'static str> {
        let method = message["method"].as_str().unwrap_or("");
        match method {
            "initialize" => {
                self.initialize = message.get("id").cloned();
                self.resource_operations = message["params"]["capabilities"]["workspace"]["workspaceEdit"]["resourceOperations"]
                    .as_array().into_iter().flatten().filter_map(Value::as_str)
                    .filter(|kind| matches!(*kind, "create" | "rename" | "delete"))
                    .map(str::to_owned).collect();
                if self.resource_operations.contains("rename") {
                    // JDT checks for all three kinds before emitting even RenameFile. Receive its
                    // complete proposals internally, then reject any unoffered effect as a whole.
                    // The native host still advertises only its real capability and checks grants,
                    // disk stamps, source revisions and preview before executing any proposal.
                    message["params"]["capabilities"]["workspace"]["workspaceEdit"]["resourceOperations"] =
                        json!(["create", "rename", "delete"]);
                }
            }
            "textDocument/rename" => {
                let id = message
                    .get("id")
                    .ok_or("Rename requires a request ID")?
                    .to_string();
                if self.renames.len() >= 32 {
                    return Err("Too many Java rename requests");
                }
                self.renames.insert(id);
            }
            "textDocument/didOpen" => {
                if let (Some(uri), Some(text)) = (
                    message["params"]["textDocument"]["uri"].as_str(),
                    message["params"]["textDocument"]["text"].as_str(),
                ) {
                    if uri.len() > 8192
                        || (!self.sources.contains_key(uri) && self.sources.len() >= 128)
                    {
                        return Err("Java document metadata limit exceeded");
                    }
                    self.sources
                        .insert(uri.into(), format!("{:x}", Sha256::digest(text.as_bytes())));
                }
            }
            "textDocument/didChange" => {
                if let Some(uri) = message["params"]["textDocument"]["uri"].as_str() {
                    self.sources.remove(uri);
                    if let Some(changes) = message["params"]["contentChanges"].as_array() {
                        if let Some(change) = changes
                            .last()
                            .filter(|change| change.get("range").is_none())
                        {
                            if let Some(text) = change["text"].as_str() {
                                self.sources.insert(
                                    uri.into(),
                                    format!("{:x}", Sha256::digest(text.as_bytes())),
                                );
                            }
                        }
                    }
                }
            }
            "textDocument/didClose" => {
                if let Some(uri) = message["params"]["textDocument"]["uri"].as_str() {
                    self.sources.remove(uri);
                }
            }
            "$/cancelRequest" => {
                if let Some(id) = message["params"].get("id") {
                    self.diagnostics.remove(&id.to_string());
                    self.renames.remove(&id.to_string());
                }
            }
            "textDocument/diagnostic" => {
                let uri = message["params"]["textDocument"]["uri"]
                    .as_str()
                    .ok_or("Missing diagnostic URI")?
                    .to_owned();
                let hash = self
                    .sources
                    .get(&uri)
                    .ok_or("Java source is not synchronized")?
                    .clone();
                let id = message
                    .get("id")
                    .ok_or("Diagnostic request requires an ID")?
                    .to_string();
                if self.diagnostics.len() >= 32 {
                    return Err("Too many Java diagnostic requests");
                }
                self.diagnostics.insert(id, hash.clone());
                message["method"] = json!("workspace/executeCommand");
                message["params"] =
                    json!({"command":"nanobug.java.diagnostics","arguments":[uri,hash]});
            }
            _ => {}
        }
        Ok(())
    }

    /// Advertise standard full sync/pull and reject reports lacking the original request's source hash.
    fn outgoing(&mut self, message: &mut Value) {
        if message.get("method").is_some() {
            return;
        }
        if message
            .get("id")
            .is_some_and(|id| self.renames.remove(&id.to_string()))
            && message["error"].is_null()
            && message["result"]["documentChanges"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|change| {
                    change.get("kind").is_some_and(|kind| {
                        kind.as_str()
                            .is_none_or(|kind| !self.resource_operations.contains(kind))
                    })
                })
        {
            let id = message["id"].clone();
            *message = json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,
                "message":"Java rename requires a resource operation the host did not offer"}});
            return;
        }
        if self
            .initialize
            .as_ref()
            .is_some_and(|id| message.get("id") == Some(id))
            && message["error"].is_null()
        {
            let capabilities = &mut message["result"]["capabilities"];
            if !capabilities["experimental"].is_object() {
                capabilities["experimental"] = json!({});
            }
            capabilities["experimental"]["nanobugControl"] =
                json!({"version":1,"methods":["java/project/ready"]});
            capabilities["diagnosticProvider"] =
                json!({"interFileDependencies":true,"workspaceDiagnostics":false});
            if !capabilities["textDocumentSync"].is_object() {
                capabilities["textDocumentSync"] = json!({"openClose":true});
            }
            capabilities["textDocumentSync"]["change"] = json!(1);
            self.initialize = None;
        }
        if let Some(expected) = message
            .get("id")
            .and_then(|id| self.diagnostics.remove(&id.to_string()))
        {
            if message["error"].is_null()
                && (message["result"]["sourceHash"] != expected
                    || message["result"]["kind"] != "full"
                    || !message["result"]["items"].is_array())
            {
                let id = message["id"].clone();
                *message = json!({"jsonrpc":"2.0","id":id,"error":{"code":-32801,"message":"Java diagnostic snapshot changed"}});
            }
        }
    }
}

/// Decode at most 16 MiB of JSON behind bounded standard LSP headers; EOF closes this direction.
pub(crate) fn read_frame(input: &mut impl BufRead) -> Result<Option<Value>> {
    let mut length = None;
    let mut headers = 0usize;
    loop {
        let mut bytes = Vec::new();
        if (&mut *input).take(8193).read_until(b'\n', &mut bytes)? == 0 {
            return Ok(None);
        }
        headers += bytes.len();
        ensure!(headers <= 8192, "Java LSP header limit exceeded");
        let line = std::str::from_utf8(&bytes)?;
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("Content-Length") {
                ensure!(length.is_none(), "Duplicate Java LSP Content-Length");
                length = Some(value.trim().parse::<usize>()?);
            }
        }
    }
    let length = length.context("Missing Java LSP Content-Length")?;
    if length > 16 * 1024 * 1024 {
        bail!("Java LSP message limit exceeded");
    }
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

/// Serialize one complete frame to `output`; one writer lock covers headers, body and flush.
pub(crate) fn write_frame(output: &mut impl Write, message: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(message)?;
    write!(output, "Content-Length: {}\r\n\r\n", bytes.len())?;
    output.write_all(&bytes)?;
    output.flush()?;
    Ok(())
}
