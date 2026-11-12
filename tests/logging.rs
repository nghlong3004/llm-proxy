use llm_proxy::logging::RotatingFile;
use std::io::Write;

#[test]
fn logs_rotate_at_record_boundaries_with_three_private_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = RotatingFile::new(dir.path(), 80).unwrap();
    for request in 0..30 {
        let line = format!("{{\"request_id\":{request},\"event\":\"completed\"}}\n");
        writer.write_all(line.as_bytes()).unwrap();
    }
    writer.flush().unwrap();
    let files = std::fs::read_dir(dir.path())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(files.len(), 3);
    for entry in files {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        assert!(text.ends_with('\n'));
        for line in text.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["event"], "completed");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                entry.metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
#[test]
fn background_json_logger_flushes_complete_records_on_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let writer = RotatingFile::new(dir.path(), 400).unwrap();
    let (nonblocking, guard) = tracing_appender::non_blocking(writer);
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .with_writer(nonblocking)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        for request_id in 0..20 {
            tracing::info!(request_id, event = "request_completed", output_tokens = 10);
        }
    });
    drop(guard);
    let mut count = 0;
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["fields"]["event"], "request_completed");
            count += 1;
        }
    }
    assert!(count > 0 && count < 20);
}
