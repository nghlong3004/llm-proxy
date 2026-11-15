# Conventions

- Rust 1.100.0, edition 2024, rustfmt width 100. Format macro bodies manually when
  rustfmt leaves dense statements. Follow responsibility-based `snake_case` modules.
- CLI uses `resource action`: `accounts add <provider> --name NAME`,
  `accounts status [provider]`, `accounts reset-quota <provider> <ACCOUNT_ID>`,
  and `accounts remove <ACCOUNT_ID_OR_PREFIX>`. Removal accepts full UUIDs or unique
  prefixes of at least 8 characters, resolved atomically across all providers;
  quota resets require full UUIDs. Legacy `accounts login/list` and top-level
  `login/status/reset-quota` remain compatible but hidden from normal help.
  Flags use `kebab-case`; TOML uses `snake_case`; finite choices use enums.
  Preserve provider wire field/URL spelling. Architecture owns the module map.
- `accounts status` defaults to offline cached reads; `--refresh` explicitly requests live data.
  `--output-format json` serializes the public status DTO, never raw account storage.
  Unknown/unsupported metrics are not zero; provider windows/capabilities vary.
  Refresh failures preserve cache and return nonzero after printing the report.
- `accounts reset-quota` previews unless `--yes` confirms consuming a banked reset, not buying
  prepaid credits or waiting for a scheduled reset. Mutations are never automatically
  retried. An uncertain outcome may have consumed a reset: explicitly reuse its request
  ID with `--yes --retry REQUEST_ID`; provider idempotency retention is not guaranteed.
  Reset/reconciliation/follow-up errors and uncertain outcomes return nonzero.
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
