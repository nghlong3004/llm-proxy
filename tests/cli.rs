use std::process::Command;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_llm-proxy"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn resource_commands_and_flags_match_documented_convention() {
    for (args, expected) in [
        (vec!["--help"], vec!["accounts", "serve", "--state-dir"]),
        (
            vec!["accounts", "login", "--help"],
            vec!["--name", "--login-method", "device-code"],
        ),
        (
            vec!["accounts", "list", "--help"],
            vec!["--output-format", "json"],
        ),
        (
            vec!["serve", "--help"],
            vec!["--default-model", "--listen-port"],
        ),
    ] {
        let output = cli(&args);
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        for flag in expected {
            assert!(text.contains(flag), "missing {flag}: {text}");
        }
    }
}

#[test]
fn listing_empty_private_state_needs_no_model_or_credentials_and_creates_no_logs() {
    let dir = tempfile::tempdir().unwrap();
    let output = cli(&[
        "--state-dir",
        dir.path().to_str().unwrap(),
        "accounts",
        "list",
        "--output-format",
        "json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["accounts"], serde_json::json!([]));
    assert!(!dir.path().join("logs").exists());
}

#[test]
fn example_config_and_codex_endpoint_remain_valid_after_rust_module_renames() {
    let config: llm_proxy::config::Config =
        toml::from_str(include_str!("../config.example.toml")).unwrap();
    config.validate().unwrap();
    assert_eq!(config.default_provider, "codex");
    assert_eq!(
        config.codex_endpoint,
        "https://chatgpt.com/backend-api/codex/responses"
    );
}
