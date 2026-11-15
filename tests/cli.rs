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
            vec!["accounts", "--help"],
            vec!["add", "status", "reset-quota", "remove"],
        ),
        (
            vec!["accounts", "status", "--help"],
            vec!["[PROVIDER]", "--refresh", "--output-format", "json"],
        ),
        (
            vec!["accounts", "add", "--help"],
            vec!["<PROVIDER>", "--name", "--login-method", "device-code"],
        ),
        (
            vec!["accounts", "reset-quota", "--help"],
            vec!["<PROVIDER>", "<ACCOUNT_ID>", "--yes", "--retry"],
        ),
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
            vec!["--default-model", "--listen-port", "--listen-address"],
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
fn invalid_listen_addresses_are_rejected_before_opening_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("unused-state");
    let output = cli(&[
        "--state-dir",
        state.to_str().unwrap(),
        "serve",
        "--listen-address",
        "not-an-ip",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--listen-address"));
    assert!(!state.exists());
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
    assert!(!dir.path().join("reasoning.key").exists());
}

const CODEX_ID: &str = "11111111-1111-4111-8111-111111111111";
const OTHER_ID: &str = "22222222-2222-4222-8222-222222222222";
const GENERATION: &str = "33333333-3333-4333-8333-333333333333";

fn synthetic_registry(state: &std::path::Path) -> serde_json::Value {
    let accounts = [("codex", CODEX_ID), ("future-provider", OTHER_ID)]
        .into_iter()
        .map(|(provider, id)| {
            serde_json::json!({
                "provider": provider,
                "id": id,
                "name": format!("{provider}\u{1b}[2J\nlabel"),
                "subject": "synthetic-user-private",
                "workspace": "synthetic-workspace-private",
                "generation": GENERATION,
                "status": {"kind": "available"},
                "usage": {
                    "credits": {"balance": "0", "unlimited": false},
                    "legacy-marker": 17,
                    "unrecognized-private-field": "not-public"
                },
                "usage_observation": {"source": "inference", "observed_at": 1},
                "status_revision": 7
            })
        })
        .collect::<Vec<_>>();
    let registry = serde_json::json!({
        "version": 1,
        "accounts": accounts,
        "active": {"codex": CODEX_ID, "future-provider": OTHER_ID},
        "operation_sequence": 3,
        "identity_operations": {"synthetic-identity": 3}
    });
    std::fs::write(
        state.join("accounts.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    registry
}

#[test]
fn new_login_requires_provider_and_rejects_unsupported_provider_before_opening_state() {
    for args in [
        vec!["accounts", "add", "--name", "test"],
        vec!["accounts", "add", "not-a-provider", "--name", "test"],
        vec!["login", "--name", "test"],
        vec!["login", "not-a-provider", "--name", "test"],
    ] {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("unused-state");
        let output = Command::new(env!("CARGO_BIN_EXE_llm-proxy"))
            .arg("--state-dir")
            .arg(&state)
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!state.exists());
    }
}

#[test]
fn invalid_reset_account_id_is_rejected_before_opening_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("unused-state");
    let output = cli(&[
        "--state-dir",
        state.to_str().unwrap(),
        "accounts",
        "reset-quota",
        "codex",
        "../../invalid",
        "--yes",
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid account ID"));
    assert!(!state.exists());
}

#[test]
fn status_known_empty_provider_has_no_fake_rows_and_ignores_invalid_model_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "not = [valid TOML").unwrap();
    let output = cli(&[
        "--state-dir",
        dir.path().to_str().unwrap(),
        "accounts",
        "status",
        "codex",
        "--output-format",
        "json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"accounts": [], "has_errors": false})
    );
    assert!(!dir.path().join("logs").exists());
    assert!(!dir.path().join("reasoning.key").exists());
}

#[test]
fn status_default_is_offline_with_missing_or_unreadable_synthetic_credentials() {
    let dir = tempfile::tempdir().unwrap();
    synthetic_registry(dir.path());
    std::fs::write(dir.path().join("config.toml"), "invalid = [configuration").unwrap();
    // A directory at one credential filename is unreadable as JSON; the other is absent.
    std::fs::create_dir_all(
        dir.path()
            .join("credentials")
            .join(format!("{CODEX_ID}-{GENERATION}.json")),
    )
    .unwrap();
    let output = cli(&[
        "--state-dir",
        dir.path().to_str().unwrap(),
        "accounts",
        "status",
        "--output-format",
        "json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let accounts = value["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
    assert!(accounts.iter().all(|account| account["active"] == true));
    let text = String::from_utf8(output.stdout).unwrap();
    for private in [
        "synthetic-user-private",
        "synthetic-workspace-private",
        GENERATION,
        "identity_operations",
        "operation_sequence",
        "unrecognized-private-field",
        "not-public",
        "usage_observation",
        "status_revision",
    ] {
        assert!(!text.contains(private), "leaked {private}: {text}");
    }
    assert!(!text.contains('\u{1b}'));
    assert!(!dir.path().join("logs").exists());
    assert!(!dir.path().join("reasoning.key").exists());
}

#[test]
fn status_prints_its_public_report_before_returning_nonzero_on_cached_decode_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = synthetic_registry(dir.path());
    registry["accounts"][0]["usage"] = serde_json::json!({"invalid-cache": true});
    std::fs::write(
        dir.path().join("accounts.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    let output = cli(&[
        "--state-dir",
        dir.path().to_str().unwrap(),
        "accounts",
        "status",
        "--output-format",
        "json",
    ]);
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["has_errors"], true);
    assert_eq!(value["accounts"].as_array().unwrap().len(), 2);
    assert_eq!(value["accounts"][0]["refresh_error"], "protocol");
    assert_eq!(value["accounts"][1]["provider"], "future-provider");
    assert!(!dir.path().join("logs").exists());
    assert!(!dir.path().join("reasoning.key").exists());
}

#[test]
fn status_provider_filter_is_exact_and_can_select_existing_future_provider() {
    let dir = tempfile::tempdir().unwrap();
    synthetic_registry(dir.path());
    for provider in ["codex", "future-provider"] {
        let output = cli(&[
            "--state-dir",
            dir.path().to_str().unwrap(),
            "accounts",
            "status",
            provider,
            "--output-format",
            "json",
        ]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["accounts"].as_array().unwrap().len(), 1);
        assert_eq!(value["accounts"][0]["provider"], provider);
    }
    for provider in ["CODEX", "unknown-provider"] {
        let output = cli(&[
            "--state-dir",
            dir.path().to_str().unwrap(),
            "accounts",
            "status",
            provider,
        ]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Unknown provider"));
    }
}

#[test]
fn status_table_labels_actual_accounts_and_sanitizes_terminal_sequences() {
    let dir = tempfile::tempdir().unwrap();
    synthetic_registry(dir.path());
    let output = cli(&[
        "--state-dir",
        dir.path().to_str().unwrap(),
        "accounts",
        "status",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Provider: codex"));
    assert!(text.contains("Provider: future-provider"));
    assert_eq!(text.matches("* active").count(), 2);
    assert!(text.contains("cached"));
    assert!(text.contains("source=inference"));
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains("\nlabel"));
    assert!(!text.contains("0.0%"));
}

#[test]
fn legacy_list_json_retains_old_envelope_and_raw_usage_without_new_fields() {
    let dir = tempfile::tempdir().unwrap();
    let mut expected = synthetic_registry(dir.path());
    for account in expected["accounts"].as_array_mut().unwrap() {
        account.as_object_mut().unwrap().remove("usage_observation");
        account.as_object_mut().unwrap().remove("status_revision");
    }
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
    assert_eq!(value, expected);
}

#[test]
fn legacy_remove_still_removes_the_requested_synthetic_account() {
    let dir = tempfile::tempdir().unwrap();
    synthetic_registry(dir.path());
    let output = cli(&[
        "--state-dir",
        dir.path().to_str().unwrap(),
        "accounts",
        "remove",
        CODEX_ID,
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = cli(&[
        "--state-dir",
        dir.path().to_str().unwrap(),
        "accounts",
        "list",
        "--output-format",
        "json",
    ]);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(value["accounts"][0]["id"], OTHER_ID);
}

#[test]
fn account_shortcuts_remain_compatible_but_main_help_shows_only_canonical_groups() {
    let help = String::from_utf8(cli(&["--help"]).stdout).unwrap();
    assert!(
        !help
            .lines()
            .any(|line| line.trim_start().starts_with("status "))
    );
    assert!(!help.contains("reset-quota"));
    let dir = tempfile::tempdir().unwrap();
    synthetic_registry(dir.path());
    let state = dir.path().to_str().unwrap();
    let canonical = cli(&[
        "--state-dir",
        state,
        "accounts",
        "status",
        "codex",
        "--output-format",
        "json",
    ]);
    let legacy = cli(&[
        "--state-dir",
        state,
        "status",
        "codex",
        "--output-format",
        "json",
    ]);
    assert!(canonical.status.success());
    assert!(legacy.status.success());
    assert_eq!(canonical.stdout, legacy.stdout);
    for args in [
        vec!["login", "--help"],
        vec!["reset-quota", "--help"],
        vec!["accounts", "status", "--refresh", "--help"],
    ] {
        assert!(cli(&args).status.success());
    }
}

#[test]
fn account_status_refresh_routes_to_existing_workflow_without_touching_unsupported_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let expected = synthetic_registry(dir.path());
    let output = cli(&[
        "--state-dir",
        dir.path().to_str().unwrap(),
        "accounts",
        "status",
        "future-provider",
        "--refresh",
        "--output-format",
        "json",
    ]);
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["accounts"][0]["refresh_error"], "unsupported");
    assert_eq!(report["accounts"].as_array().unwrap().len(), 1);
    let actual: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("accounts.json")).unwrap()).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn remove_accepts_unique_eight_character_prefix() {
    let dir = tempfile::tempdir().unwrap();
    synthetic_registry(dir.path());
    let output = cli(&[
        "--state-dir",
        dir.path().to_str().unwrap(),
        "accounts",
        "remove",
        &CODEX_ID[..8],
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let registry: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("accounts.json")).unwrap()).unwrap();
    assert_eq!(registry["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(registry["accounts"][0]["id"], OTHER_ID);
    assert!(registry["active"].get("codex").is_none());
}

#[test]
fn remove_rejects_ambiguous_prefix_across_providers_until_extended() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = synthetic_registry(dir.path());
    let collision = "11111111-2222-4222-8222-222222222222";
    registry["accounts"][1]["id"] = serde_json::json!(collision);
    registry["active"]["future-provider"] = serde_json::json!(collision);
    let path = dir.path().join("accounts.json");
    std::fs::write(&path, serde_json::to_vec(&registry).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let state = dir.path().to_str().unwrap();
    let ambiguous = cli(&["--state-dir", state, "accounts", "remove", "11111111"]);
    assert!(!ambiguous.status.success());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("ambiguous"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let precise = cli(&["--state-dir", state, "accounts", "remove", "11111111-1111"]);
    assert!(precise.status.success());
    let remaining: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(remaining["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(remaining["accounts"][0]["id"], collision);
}

#[test]
fn remove_rejects_short_invalid_or_unknown_prefix_without_changing_registry() {
    let dir = tempfile::tempdir().unwrap();
    synthetic_registry(dir.path());
    let path = dir.path().join("accounts.json");
    let before = std::fs::read(&path).unwrap();
    for prefix in ["1111111", "ffffffff", "../credentials", "not-an-id"] {
        let output = cli(&[
            "--state-dir",
            dir.path().to_str().unwrap(),
            "accounts",
            "remove",
            prefix,
        ]);
        assert!(!output.status.success());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn reset_quota_still_requires_full_uuid_not_removal_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("unused-state");
    for args in [
        vec!["accounts", "reset-quota", "codex", &CODEX_ID[..8], "--yes"],
        vec!["reset-quota", "codex", &CODEX_ID[..8], "--yes"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_llm-proxy"))
            .arg("--state-dir")
            .arg(&state)
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid account ID"));
        assert!(!state.exists());
    }
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
