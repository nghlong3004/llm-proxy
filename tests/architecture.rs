//! Keep the dependency direction explicit after the hexagonal extraction.
use std::{
    fs,
    path::{Path, PathBuf},
};
fn rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}
#[test]
fn inner_layers_never_import_concrete_adapters_bootstrap_or_http_libraries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for layer in ["domain", "application", "ports"] {
        let mut files = vec![];
        rust_files(&root.join("src").join(layer), &mut files);
        assert!(!files.is_empty());
        for file in files {
            let source = fs::read_to_string(&file).unwrap();
            for forbidden in ["axum::", "reqwest::", "adapters::", "bootstrap::"] {
                assert!(
                    !source.contains(forbidden),
                    "{} depends outward on {forbidden}",
                    file.display()
                );
            }
        }
    }
}
#[test]
fn oauth_lifecycle_and_provider_wire_flows_stay_separate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/authentication");
    let lifecycle = fs::read_to_string(root.join("oauth.rs")).unwrap();
    for wire_detail in [
        "auth.openai.com",
        "oauth2.googleapis.com",
        "deviceauth",
        "code_challenge",
    ] {
        assert!(
            !lifecycle.contains(wire_detail),
            "Shared OAuth lifecycle contains {wire_detail}"
        );
    }
    for provider in ["codex.rs", "agy.rs"] {
        let source = fs::read_to_string(root.join(provider)).unwrap();
        for mutation in [
            "finish_refresh_for_generation(",
            "mark_needs_login(",
            "try_refresh_lock(",
        ] {
            assert!(
                !source.contains(mutation),
                "{provider} bypasses shared OAuth lifecycle with {mutation}"
            );
        }
    }
}

#[test]
fn old_root_aliases_are_not_used_to_hide_adapters() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let exports = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    for old in ["cli", "protocols", "providers", "server"] {
        assert!(!root.join("src").join(old).exists());
        assert!(!exports.contains(&format!("pub mod {old};")));
    }
    let inbound = fs::read_to_string(root.join("src/adapters/inbound/cli/mod.rs")).unwrap();
    assert!(!inbound.contains("AuthClient"));
    assert!(!inbound.contains("Codex::"));
    assert!(!inbound.contains("axum::"));
}
