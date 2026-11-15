# Conventions

- Rust 1.100.0, edition 2024, rustfmt width 100. Format macro bodies manually when
  rustfmt leaves dense statements. Follow responsibility-based `snake_case` modules.
- CLI: `resource action`; flags: `kebab-case`; TOML: `snake_case`; finite choices: enums.
  Preserve provider wire field/URL spelling. Architecture owns the module map.
- State: `--state-dir` or `LLM_PROXY_STATE_DIR`; default `$XDG_STATE_HOME/llm-proxy`
  or `~/.local/state/llm-proxy`. Commands sharing accounts must use the same state.
- Keep configurable defaults in `config.rs` and `config.example.toml`. Client env
  belongs to Claude Code settings, not the proxy's process environment.

## Checks

`./scripts/check-local.sh` runs fmt, strict Clippy, all tests including ignored
localhost contracts, and a release build. Use `CARGO_HOME=/tmp/llm-proxy-cargo` and
`--offline` when relying on this environment's dependency cache.

```sh
docker compose config --quiet
docker compose build
python3 -I scripts/check-docker.py
python3 -I scripts/check-auto-review.py
python3 -I scripts/check-resources.py --seconds 180 --workers 8
```

Scripts isolate state/accounts/upstreams; classifier smoke tests fail-closed behavior,
not model safety. Exit 2 means blocked/inconclusive. Resource measurements are finite,
not proof of leak freedom. Never use real credentials/quota without authorization.

## Provenance

Codex compatibility references `raine/claude-code-proxy`; account constraints reference
`kapybara-org/zerosub`. Ported schema source/revision is recorded in
`src/adapters/outbound/codex/schema.rs` and [fixture provenance](../tests/fixtures/codex/README.md).
Preserve the [MIT license](../LICENSE) and pinned reference generation instructions.
