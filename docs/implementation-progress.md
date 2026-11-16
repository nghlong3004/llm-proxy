# Current checkpoint

## State

- Documentation is reorganized around a provider-neutral local LLM gateway for coding
  agents. The root README is English; `docs/README.vi.md` is its Vietnamese overview.
  Actual compatibility remains explicit: Anthropic Messages inbound, Codex/AGY
  outbound, with Claude Code as a documented client example rather than universal support.
- Dedicated English installation, updating and configuration guides retain native,
  systemd and Docker setup; model/account/classifier configuration; quota-reset
  uncertainty; private state; backup/rollback and current AGY limitations.
  `docs/ubuntu-setup.md` remains a compatibility entry point linking to those guides.
- Root README visibly includes Installation, Quick start, Configuration and Updating,
  with links to the detailed guides. Security limits and MIT attribution are preserved.
- Runtime implementation/config defaults/packaging are unchanged. AGY still lazily
  resolves its OAuth client from a hash-pinned public reference and private cache;
  explicit effort/structured output and other unsupported controls still fail closed.
- GitHub About/description metadata has not been changed. No existing credentials,
  installed binaries, services or authenticated provider integration were touched.

## Verification

| Check | Result |
| --- | --- |
| Local Markdown links/anchors and balanced code fences | PASS: 55 local links/anchors across the documentation entry points |
| Fenced configuration/command examples | PASS: 6 TOML, 1 JSON, 29 shell blocks parse; shell syntax only, no install/service/auth commands executed |
| CLI integration regressions, locked/offline using default dependency cache | PASS: 23 tests (19 CLI + 4 AGY CLI) |
| `cargo fmt --check` and diff whitespace checks | PASS |
| Full Clippy/tests/release and Docker image/smoke | NOT RERUN: documentation-only change; prior runtime checkpoint passed 257 tests and release/Clippy checks |
| Initial alternate dependency cache attempt | BLOCKED: `/tmp/llm-proxy-cargo` lacks index/dependencies; default offline cache succeeded |
| Installation/update execution and authenticated live integration | NOT RUN: only documented, no services or real account operations changed |

Guides were checked against the current CLI, config example, Compose file, image
state layout and systemd unit. Code/config examples are illustrative; model IDs and
local readiness do not prove provider access or entitlement. OAuth client values,
account tokens and request payloads are not included in the documentation.

## Next

- Review the rendered README and guides on GitHub; keep English/Vietnamese entry
  points and command/configuration examples aligned with future behavior changes.
- GitHub description suggestion: "A local LLM gateway for coding agents, with
  model-based routing, native providers, and multi-account failover."
- Binary installation, service changes and authenticated live tests remain separate
  actions requiring authorization. Existing installed binaries keep their current behavior.
