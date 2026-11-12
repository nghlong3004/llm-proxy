#!/usr/bin/env sh
# Includes only mock HTTP traffic on localhost; no provider credentials or quota.
set -eu
cd "$(dirname "$0")/.."
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked -- --include-ignored
cargo build --release --locked
