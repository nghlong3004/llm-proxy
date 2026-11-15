//! AGY CLI contracts with disposable state; no browser or live authentication.
use std::process::{Command, Output};

fn cli(state: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_llm-proxy"))
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn agy_status_is_known_offline_even_without_accounts_or_configuration() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "invalid config!").unwrap();
    let output = cli(
        dir.path(),
        &["accounts", "status", "agy", "--output-format", "json"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["accounts"], serde_json::json!([]));
    assert!(!dir.path().join("logs").exists());
    assert!(!dir.path().join("reasoning.key").exists());
}

#[test]
fn agy_missing_telemetry_and_live_refresh_are_explicitly_unsupported() {
    use llm_proxy::accounts::{Credential, Store};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let account = store
        .login_for(
            "agy",
            "synthetic",
            Credential {
                access_token: "synthetic-access".into(),
                refresh_token: "synthetic-refresh".into(),
                id_token: None,
                expires_at: u64::MAX,
                subject: "synthetic-user".into(),
                account_id: "synthetic-project".into(),
                generation: String::new(),
            },
        )
        .unwrap();
    // Status must not need any credential, including when an unsupported refresh
    // is requested. Invalid config must not initialize an unrelated provider.
    std::fs::remove_file(
        dir.path()
            .join("credentials")
            .join(format!("{}-{}.json", account.id, account.generation)),
    )
    .unwrap();
    std::fs::write(dir.path().join("config.toml"), "invalid config!").unwrap();
    for refresh in [false, true] {
        let mut args = vec!["accounts", "status", "agy", "--output-format", "json"];
        if refresh {
            args.push("--refresh");
        }
        let output = cli(dir.path(), &args);
        assert_eq!(output.status.success(), !refresh);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let account = &value["accounts"][0];
        assert_eq!(account["usage"]["windows"]["state"], "unsupported");
        assert_eq!(account["capabilities"]["live_read"], false);
        if refresh {
            assert_eq!(account["refresh_error"], "unsupported");
        }
    }
}

#[test]
fn agy_device_login_is_rejected_without_starting_authentication() {
    let dir = tempfile::tempdir().unwrap();
    let output = cli(
        dir.path(),
        &[
            "accounts",
            "add",
            "agy",
            "--name",
            "synthetic",
            "--login-method",
            "device-code",
        ],
    );
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("does not support device-code"), "{error}");
    assert!(!dir.path().join("accounts.json").exists());
}

#[test]
fn agy_banked_reset_is_unsupported_without_loading_configuration_or_credentials() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "invalid config!").unwrap();
    let output = cli(
        dir.path(),
        &[
            "accounts",
            "reset-quota",
            "agy",
            "11111111-1111-4111-8111-111111111111",
            "--yes",
        ],
    );
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.to_lowercase().contains("unsupported") || error.contains("not support"),
        "{error}"
    );
    assert!(!dir.path().join("accounts.json").exists());
}
