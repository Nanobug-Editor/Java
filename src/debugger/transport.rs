//! Java-owned DAP policy adapted from Nanobug Rust; MIT notice is in licenses/nanobug-rust-LICENSE.txt.
use plugin_protocol::{
    api::{self, ErrorCode, Failure, ResourceHandle},
    process,
};
use serde_json::{Value, json};

/// One adapter connection; partial headers and bodies stay within the declared receive budget.
#[derive(Default)]
pub struct Transport {
    bytes: Vec<u8>,
    sequence: u32,
}
impl Transport {
    /// Reverse requests are answered on the protocol channel, never on target stdin.
    pub fn response(
        &mut self,
        handle: &ResourceHandle,
        request: &Value,
        result: Result<Value, String>,
    ) -> Result<(), Failure> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| failure("DAP sequence exhausted"))?;
        let mut message = json!({"seq":self.sequence,"type":"response","request_seq":request["seq"],"command":request["command"],"success":result.is_ok()});
        match result {
            Ok(body) => message["body"] = body,
            Err(error) => message["message"] = json!(error),
        }
        let payload = serde_json::to_vec(&message).map_err(|error| failure(error.to_string()))?;
        let mut bytes = format!("Content-Length: {}\r\n\r\n", payload.len()).into_bytes();
        bytes.extend(payload);
        api::guest::request(api::Operation::Process {
            operation: process::Operation::Write {
                handle: handle.clone(),
                bytes,
            },
        })?;
        Ok(())
    }
    /// Write one literal DAP request to this owned service and return its correlation number.
    pub fn send(
        &mut self,
        handle: &ResourceHandle,
        command: &str,
        arguments: Value,
    ) -> Result<u32, Failure> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| failure("DAP sequence exhausted"))?;
        let payload = serde_json::to_vec(
            &json!({"seq":self.sequence,"type":"request","command":command,"arguments":arguments}),
        )
        .map_err(|error| failure(error.to_string()))?;
        if payload.len() > 64 * 1024 {
            return Err(failure("DAP request exceeds the byte budget"));
        }
        let mut bytes = format!("Content-Length: {}\r\n\r\n", payload.len()).into_bytes();
        bytes.extend(payload);
        api::guest::request(api::Operation::Process {
            operation: process::Operation::Write {
                handle: handle.clone(),
                bytes,
            },
        })?;
        Ok(self.sequence)
    }
    /// Decode only complete messages; a malformed or oversized adapter cannot grow guest memory.
    pub fn receive(&mut self, chunk: &[u8]) -> Result<Vec<Value>, Failure> {
        if self.bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
            return Err(failure("DAP receive budget exceeded"));
        }
        self.bytes.extend_from_slice(chunk);
        let mut messages = Vec::new();
        loop {
            let Some(header_end) = self.bytes.windows(4).position(|part| part == b"\r\n\r\n")
            else {
                if self.bytes.len() > 8192 {
                    return Err(failure("DAP header exceeds the byte budget"));
                }
                break;
            };
            if header_end > 8192 {
                return Err(failure("DAP header exceeds the byte budget"));
            }
            let header = std::str::from_utf8(&self.bytes[..header_end])
                .map_err(|_| failure("Non-text DAP header"))?;
            let lengths: Vec<_> = header
                .lines()
                .filter_map(|line| line.split_once(':'))
                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .collect();
            if lengths.len() != 1 {
                return Err(failure("DAP requires one Content-Length"));
            }
            let length: usize = lengths[0]
                .1
                .trim()
                .parse()
                .map_err(|_| failure("Invalid DAP length"))?;
            if length > 1024 * 1024 - 8196 {
                return Err(failure("DAP message exceeds the byte budget"));
            }
            let end = header_end + 4 + length;
            if self.bytes.len() < end {
                break;
            }
            let value = serde_json::from_slice(&self.bytes[header_end + 4..end])
                .map_err(|error| failure(error.to_string()))?;
            self.bytes.drain(..end);
            messages.push(value);
            if messages.len() > 128 {
                return Err(failure("DAP message batch exceeds the budget"));
            }
        }
        Ok(messages)
    }
}
/// All transport failures retain a visible reason instead of synthesizing a debug state.
pub fn failure(message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::OperationFailed, message)
}
