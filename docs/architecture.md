# Architecture

Single Rust crate with hexagonal dependency boundaries:

```text
inbound adapters → application → domain + ports
outbound adapters implement ports; bootstrap composes implementations
```

| Module | Responsibility |
| --- | --- |
| `bootstrap/` | Concrete wiring, configuration, logging and process lifecycle |
| `domain/` | Neutral generation/account DTOs, clock, errors and buffered accumulator |
| `application/` | Generation preflight, routing, failover, token count, account/usage/reset use cases |
| `ports/` | Protocol/provider/routing/estimator contracts and focused account repositories |
| `adapters/inbound/cli/` | Clap parsing and output formatting |
| `adapters/inbound/http/` | HTTP parsing, admission, SSE/buffered delivery, cancellation |
| `adapters/inbound/http/anthropic/` | Anthropic Messages translation and SSE encoding |
| `adapters/outbound/codex/` | Codex client, translation, quota, reasoning, search, tokenizer |
| `adapters/outbound/agy/` | Native Antigravity client, translation, bounded stream, scoped thinking |
| `adapters/outbound/{http,sse}.rs` | Shared bounded body reading and SSE JSON framing |
| `accounts/` | Filesystem repository implementations, atomic persistence, selection and leases |
| `authentication/` | Shared refresh lifecycle; separate Codex and Google OAuth wire flows |
| `config.rs`, `logging/` | Model/config registry and bounded metadata logging |

`domain/application/ports` never import adapters, bootstrap, concrete account storage
or HTTP/filesystem libraries. Neutral account/status/selection contracts and the clock
live in domain. Application depends on three synchronous repository ports (inference,
account commands, usage/reset); blocking implementations remain behind `spawn_blocking`.
Provider ports receive a narrow account identity, not the serialized registry record.
`bootstrap::http_state` composes injectable components; architecture tests guard
these dependencies. Both response modes consume the same neutral EventStream.

## Request lifecycle

1. HTTP acquires admission before parsing JSON, then decodes through ProtocolAdapter.
2. Application recognizes the client model, resolves one immutable canonical route
   (provider/model/typed execution options), validates generation capabilities and
   selects the first eligible account through the inference repository.
3. Failover preserves that same route; Provider revalidates credentials/generation
   and consumes resolved options without consulting global routing again.
4. HTTP encodes events or accumulates a response. Drop cancels inference; blocking
   jobs retain work permits until completion.

Action-review detection belongs to the Anthropic adapter: non-streaming, no tools,
exact system prefix. Policy uses neutral RequestPurpose. Reviews retain transcript,
schema and requested effort but omit session/cache identity and encrypted replay;
response model identity remains the client's. Token count uses an injected estimator
and does not apply review routing. Defaults live in config, not provider code.
Provider routing precedence is explicit review provider, then the effective canonical
model's provider policy, then `default_provider`; aliases select a model, not an account.
Explicit native Claude model registrations override generic Claude-family aliases.
Provider overrides are applied before Codex service-tier suffix normalization; the
original request model remains the client-facing response identity. Omitted reasoning
effort differs from an explicit finite effort value: Codex applies the legacy medium
default centralized in config, while AGY currently rejects explicit effort before account selection.
Token count only recognizes the client model; it does not run generation capability
validation. AGY capabilities and provenance are documented in [antigravity](antigravity.md).

## Account lifecycle

- Identity is user + workspace; selection is provider-scoped, sticky and insertion-ordered.
- Login reserves a sequence before authentication. Credentials publish before registry;
  generation checks/tombstones reject stale login, refresh and quota updates.
- Remove unpublishes before deletion; re-login publishes before deleting old generations.
  Existing leases retain credentials in memory; cleanup failure does not roll back login.
- Registry locks cover atomic filesystem updates only. Account refresh locks span the
  refresh transaction, which survives client cancellation and drains on shutdown.
  `authentication/oauth.rs` owns this lifecycle and generation-safe publication;
  `codex.rs` and `agy.rs` own their grants, wire formats and revocation semantics.
  AGY lazily resolves its OAuth client from an integrity-pinned, data-only public
  reference cached in the selected private state; a complete environment pair can
  override it. No client values are embedded in source/fixtures, and construction
  or offline status never downloads configuration.
- Confirmed quota or definite credential rejection can switch accounts before output;
  removed/changed accounts can reselect before dispatch. Never replay uncertain work
  or events already emitted. Quota reset does not displace a healthy active account.

- Quota reset locks cross the repository boundary as opaque cloneable RAII guards;
  blocking journal/reconciliation jobs retain a guard until completion even if their
  async waiter is cancelled. Raw filesystem handles never enter application code.
- Legacy account-list JSON is formatted from the concrete registry at the
  bootstrap/CLI boundary, retaining operation bookkeeping without exposing it through
  application repository snapshots. Persisted state/schema and `accounts` re-exports
  remain compatible.

## Account telemetry and manual resets

`accounts status [provider]` reads cached registry metadata without credentials, model config,
HTTP setup or eligibility changes. An optional account-usage port provides native
snapshot decoding, live reads and reset capabilities independently of inference.
Bootstrap wires it; application filters providers and orchestrates bounded refreshes;
CLI renders allowlisted DTOs. Windows are provider-defined; unknown, unsupported,
known zero, unlimited, stale observations and read errors remain distinct.

`accounts status --refresh` explicitly uses proxy-owned OAuth and stores observed usage only
for the captured generation. Authoritative live `ordinary_usage_allowed=true` also
clears a captured quota-exhausted state via generation/status-revision checks, so
resets performed outside the proxy can be recognized manually. It never clears
NeedsLogin, changes active placement, or overwrites a newer status observation.
Missing/false availability, read errors and cached observations do not recover quota;
percentages never mark exhaustion. There is no background polling. Codex uses a
separate account API base; its verified backend contract and source pin are documented
beside the adapter. No Codex CLI process is started.

`accounts reset-quota` requires one provider/account and explicit `--yes`. It consumes a banked
reset, not a prepaid-credit purchase. A private atomic journal reserves the logical
request UUID before dispatch; a separate cross-process lock serializes each account's
resets. Uncertain dispatch remains pending/unknown and blocks a new key; only an
explicit retry reuses the recorded key. Backend idempotency retention is not promised.
Confirmed results survive failed follow-up reads. Only authoritative post-reset
availability can clear captured quota via generation/status-revision compare-and-set;
it never clears revoked authentication, resurrects accounts or steals active placement.

## Resource ownership

| Resource | Bound |
| --- | --- |
| Shared generation/count admission | 32; overload returns 503 |
| Input / SSE frame / retained output | 32 / 8 / 16 MiB |
| Upload / connect / upstream idle | 30 / 15 / 300 seconds |
| SSE heartbeat | 15 seconds |
| Tokenizer jobs / tracked refresh tasks | 32 / 64 |
| Failover attempts / unknown-reset cooldown | 64 / 60 seconds |

Buffered bodies hold admission until consumed/dropped. Blocking closures hold permits
independently of their HTTP waiter. Byte/concurrency limits are not a process RAM cap.

State uses private atomic files and cross-process locks. Metadata logs use a 1024-record
queue and three approximately 10 MiB JSONL files; overload drops records. Request IDs
connect route, attempt and terminal events; upstream diagnostics are allowlisted.

Systemd/Docker deliver SIGINT with a 45-second grace period for refresh persistence.
Docker runs UID/GID 10001 with a named volume at `/var/lib/llm-proxy`; the image seeds
its private `state/` child through volume copy-up. Host publication remains loopback.
