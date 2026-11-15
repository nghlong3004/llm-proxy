//! Incremental SSE decoding; network chunk boundaries have no semantic meaning.
use crate::domain::error::ProxyError;
use serde_json::Value;
const MAX_FRAME: usize = 8 * 1024 * 1024;
#[derive(Default)]
pub struct Decoder {
    frame: Vec<u8>,
    line_start: usize,
    skip_lf: bool,
}
impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, ProxyError> {
        let mut out = vec![];
        for &byte in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' => {
                    self.line_end(&mut out)?;
                    self.skip_lf = true;
                }
                b'\n' => self.line_end(&mut out)?,
                _ => {
                    if self.frame.len() >= MAX_FRAME {
                        return Err(ProxyError::protocol("Upstream SSE frame exceeds 8 MiB"));
                    }
                    self.frame.push(byte);
                }
            }
        }
        Ok(out)
    }
    fn line_end(&mut self, out: &mut Vec<Value>) -> Result<(), ProxyError> {
        if self.frame.len() == self.line_start {
            if !self.frame.is_empty() {
                let frame = std::str::from_utf8(&self.frame)
                    .map_err(|_| ProxyError::protocol("Invalid UTF-8 in upstream SSE"))?;
                let mut data = vec![];
                for line in frame.lines() {
                    if let Some(data_line) = line.strip_prefix("data:") {
                        data.push(data_line.strip_prefix(' ').unwrap_or(data_line));
                    }
                }
                let data = data.join("\n");
                if !data.is_empty() && data != "[DONE]" {
                    out.push(
                        serde_json::from_str(&data)
                            .map_err(|_| ProxyError::protocol("Invalid JSON in upstream SSE"))?,
                    );
                }
            }
            self.frame.clear();
            self.line_start = 0;
        } else {
            self.frame.push(b'\n');
            self.line_start = self.frame.len();
        }
        Ok(())
    }
    pub fn finish(&self) -> Result<(), ProxyError> {
        if self.frame.is_empty() {
            Ok(())
        } else {
            Err(ProxyError::protocol(
                "Upstream stream ended with an incomplete SSE frame",
            ))
        }
    }
}
