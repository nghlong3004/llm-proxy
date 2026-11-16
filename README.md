# llm-proxy

A local LLM gateway for coding agents, with model-based routing, native providers,
and multi-account failover.

English · [Tiếng Việt](docs/README.vi.md)

## Overview

Connect compatible coding agents to upstream providers through one local,
Anthropic Messages-compatible endpoint. Configure where each model goes, sign in to
provider accounts, and keep the coding agent's existing workflow.

The gateway handles protocol translation, routing and account selection. **Your coding
agent still owns tool execution, filesystem access, shell commands and the agent loop.**
Claude Code is a documented client example, not the name of the inbound protocol or
a claim that every coding agent is supported.

## Features

- **Explicit model routing:** map client aliases to canonical models and providers in TOML.
- **Native provider adapters:** Codex and Antigravity (`agy`), without a Node sidecar.
- **Provider-scoped accounts:** add multiple accounts without restarting the gateway.
- **Conservative failover:** switch eligible accounts only for confirmed quota exhaustion,
  definite credential rejection or a pre-dispatch account change. No cross-provider/model
  fallback or replay after output or uncertain dispatch.
- **Streaming and buffered responses:** one neutral event pipeline for both modes.
- **Account visibility:** offline status, optional provider usage refresh, and explicit
  banked-quota-reset workflows where supported.

## Compatibility

### Client API

| Endpoint | Purpose |
| --- | --- |
| `POST /v1/messages` | Anthropic Messages-compatible generation |
| `POST /v1/messages/count_tokens` | Local token estimate; not a provider-authoritative count |
| `GET /v1/models` | Configured model/alias catalog; not an entitlement check |
| `GET /health` | Process status and account readiness |

Clients must support a configurable API base URL and the implemented Messages subset.
Other inbound protocols, including the OpenAI Chat Completions API, are not implemented.

### Providers

| Provider | Sign-in | Important limits |
| --- | --- | --- |
| Codex (`codex`) | ChatGPT browser OAuth or device code | Model access and account-service capabilities depend on the account/backend. |
| Antigravity (`agy`) | Google browser OAuth | Claude text/images/function tools; Gemini text/images only. No explicit reasoning effort, structured output, hosted search, compaction, forced tool choice, live usage or banked resets. |

AGY rejects unsupported controls rather than silently dropping them. In particular,
clients that automatically send reasoning effort cannot use that request unchanged
with AGY. Its browser callback is not published by the shipped Docker configuration.
See the [AGY reference and limitations](docs/antigravity.md).

## Installation

Build from source with **Rust 1.100.0**. Ubuntu/WSL prerequisites, native installation,
optional systemd setup and Docker instructions are in the
[installation guide](docs/installation.md).

For an interactive native run after installing the prerequisites:

```sh
git clone https://github.com/nghlong3004/llm-proxy.git
cd llm-proxy
cargo install --path . --locked --root "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
```

## Usage / quick start

Sign in to a Codex account, then start the local server:

```sh
llm-proxy accounts add codex --name personal --login-method device-code
llm-proxy serve --default-model gpt-6.1-sol
```

Follow the sign-in instructions. The model above is an example: choose one your
account can access. Device login may need to be enabled in ChatGPT settings.

In another terminal:

```sh
curl --fail http://127.0.0.1:18765/health
llm-proxy accounts status
```

Check `ready` in the health response; HTTP 200 alone does not establish model access
or upstream availability. Then point a compatible client at
`http://127.0.0.1:18765`. See the
[client configuration example](docs/configuration.md#client-configuration-example).

For native AGY sign-in, use `llm-proxy accounts add agy --name google-personal` and
configure an AGY model route before serving. OAuth client configuration is acquired
lazily from an integrity-pinned public reference; no client environment pair is
required by default. Review [its trust boundary](docs/antigravity.md#provenance) first.

## Configuration

Configuration lives in `<state-dir>/config.toml`; the native default is
`$XDG_STATE_HOME/llm-proxy` or `~/.local/state/llm-proxy`. Use the same state directory
for login, account commands and the server.

Example mixed-provider routing:

```toml
default_provider = "codex"
default_model = "gpt-6.1-sol"

[aliases]
sonnet = "claude-sonnet-4-6"

[models."claude-sonnet-4-6"]
provider = "agy"
```

Here `sonnet` routes to the registered AGY model. Unmapped generic Claude-family
aliases use `default_model`; explicit native model registrations take precedence.
The gateway does not silently substitute a different provider or model on failure.
These IDs are examples, not guarantees of backend availability.

See [configuration and accounts](docs/configuration.md) and
[config.example.toml](config.example.toml) for state selection, client setup,
classifier routing, account status and quota-reset recovery. Restart the server after
editing config; adding or removing accounts does not require a restart.

## Updating

Use the [update guide](docs/updating.md) for native, systemd and Docker deployments,
including backups, restart checks and rollback. Reuse the same state directory or
Docker volume to preserve accounts; a binary update alone does not require re-login.

## Documentation

- [Installation](docs/installation.md) — native, systemd and Docker.
- [Configuration and accounts](docs/configuration.md) — routing, clients and quota workflows.
- [Updating](docs/updating.md) — upgrades, backups and rollback.
- [Antigravity](docs/antigravity.md) — OAuth source, capabilities, provenance and caveats.
- [Architecture](docs/architecture.md) — boundaries, request lifecycle and resource ownership.
- [Development conventions](docs/conventions.md) — commands, style and reference provenance.

For help, include the version/revision, deployment method and a **redacted** error in
[an issue](https://github.com/nghlong3004/llm-proxy/issues). Never attach credentials,
OAuth codes, prompts or tool payloads.

## Security and limitations

- **Local-only listener:** no client authentication. Keep native listeners and Docker
  host publication on loopback. A client's placeholder auth token does not secure the API.
- **Sensitive state:** directories use `0700` and files use `0600` on Unix. Account
  credentials are not encrypted at rest; protect the state directory and its backups.
- **Private/internal upstream APIs:** behavior, OAuth client acceptance and account
  entitlement can change. Check provider terms and permissions; this project is not
  affiliated with or endorsed by the providers.
- **Verification boundary:** automated coverage uses synthetic/local contracts, not
  authenticated live provider validation or a guarantee of model safety.
- **Classifier errors stay errors:** unsupported requests and classifier failures never
  become fabricated approvals. Configure a supported review route when needed.

## Development / contributing

Use the pinned Rust toolchain and keep `Cargo.lock`. Before proposing changes:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked -- --include-ignored
cargo build --release --locked
```

Some ignored tests require localhost sockets; they use disposable synthetic state,
not real accounts. See [conventions](docs/conventions.md) for Docker and additional
smoke checks. Keep changes focused, add regression coverage, and do not include
credentials or real request payloads in issues, fixtures or commits.

## License

[MIT](LICENSE), including the attribution notices for referenced/ported work.
Provider access and service terms are separate from this software license.
