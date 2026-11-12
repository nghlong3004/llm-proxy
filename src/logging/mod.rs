//! Bounded metadata logging. Never attach request bodies or credential values.
mod rotation;
pub use rotation::RotatingFile;

use crate::domain::{Usage, error::ProxyError};
use anyhow::{Context, Result};
use std::{
    io::{self, Write},
    path::Path,
    time::Instant,
};
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};

/// File I/O runs on tracing-appender's worker; the guard flushes on shutdown.
pub fn initialize(state_dir: &Path, level: &str) -> Result<WorkerGuard> {
    let level: tracing::level_filters::LevelFilter = level.parse().context("Invalid log_level")?;
    let writer = RotatingFile::new(&state_dir.join("logs"), 10 * 1024 * 1024)?;
    let (writer, guard) = NonBlockingBuilder::default()
        .buffered_lines_limit(1024)
        .lossy(true)
        .thread_name("proxy-log-writer")
        .finish(ConsoleAndFile(writer));
    tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .with_current_span(false)
        .with_span_list(false)
        .with_env_filter(format!("off,llm_proxy={level}"))
        .with_writer(writer)
        .try_init()
        .map_err(|_| anyhow::anyhow!("Cannot initialize proxy logging"))?;
    Ok(guard)
}

// Both sinks run on the background worker, including stderr when redirected to disk.
struct ConsoleAndFile(RotatingFile);
impl Write for ConsoleAndFile {
    fn write(&mut self, record: &[u8]) -> io::Result<usize> {
        let file_result = self.0.write_all(record);
        let console_result = std::io::stderr().write_all(record);
        file_result?;
        console_result?;
        Ok(record.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        std::io::stderr().flush()?;
        self.0.flush()
    }
}

pub(crate) struct RequestLog {
    pub id: String,
    started: Instant,
    finished: bool,
}

impl RequestLog {
    pub fn new(id: String) -> Self {
        Self {
            id,
            started: Instant::now(),
            finished: false,
        }
    }

    pub fn complete(&mut self, usage: &Usage) {
        self.finished = true;
        tracing::info!(request_id = %self.id, elapsed_ms = self.started.elapsed().as_millis(),
            input_tokens = usage.input, output_tokens = usage.output,
            cached_tokens = usage.cached, reasoning_tokens = usage.reasoning,
            event = "request_completed");
    }

    pub fn fail(&mut self, error: &ProxyError) {
        self.finished = true;
        tracing::warn!(request_id = %self.id, elapsed_ms = self.started.elapsed().as_millis(),
            kind = ?error.kind, error_code = error.code, status = error.status, retry_after = error.retry_after,
            upstream_status = error.upstream_status, upstream_parameter = error.upstream_parameter,
            event = "request_failed");
    }
}

impl Drop for RequestLog {
    fn drop(&mut self) {
        if !self.finished {
            tracing::info!(request_id = %self.id, elapsed_ms = self.started.elapsed().as_millis(),
                event = "request_cancelled");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        sync::{Arc, Mutex},
    };

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn request_logs_contain_metadata_and_one_terminal_event_without_error_payloads() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer = Capture(bytes.clone());
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let mut completed = RequestLog::new("completed-id".into());
            completed.complete(&Usage {
                input: 9,
                output: 3,
                ..Default::default()
            });
            let mut failed = RequestLog::new("failed-id".into());
            failed.fail(&ProxyError::invalid("PROMPT_TOKEN_REASONING_SECRET"));
            let _cancelled = RequestLog::new("cancelled-id".into());
        });
        let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(!text.contains("PROMPT_TOKEN_REASONING_SECRET"));
        let values: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(values.len(), 3);
        assert_eq!(values[0]["fields"]["event"], "request_completed");
        assert_eq!(values[0]["fields"]["output_tokens"], 3);
        assert_eq!(values[1]["fields"]["event"], "request_failed");
        assert_eq!(values[1]["fields"]["kind"], "InvalidRequest");
        assert_eq!(values[1]["fields"]["error_code"], "invalid_request");
        assert_eq!(values[2]["fields"]["event"], "request_cancelled");
    }
}
