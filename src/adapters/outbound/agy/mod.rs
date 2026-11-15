//! Native Antigravity/Cloud Code provider. No external proxy or agent process is executed.
//!
//! Wire-contract reference: antigravity-claude-proxy, revision
//! 7c672fdc6fa62fcd50e8f75df5644dad6a63c2dc, src/format/request-converter.js and
//! src/cloudcode/streaming-handler.js (MIT, Copyright (c) 2024 Badri Narayanan S).
//! This implementation deliberately does not port prompt rewriting, model substitution,
//! fabricated signatures, endpoint retries, or permissive schema rewriting.
pub mod account_usage;
pub mod client;
pub mod error;
pub mod reasoning;
pub mod request;
pub mod stream;

/// Bound serialization before allocating an oversized escaped representation.
fn encode_bounded(value: &impl serde::Serialize, limit: usize) -> Option<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.bytes.len().saturating_add(bytes.len()) > self.limit {
                return Err(std::io::Error::other("AGY JSON exceeds size limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).ok()?;
    Some(writer.bytes)
}
