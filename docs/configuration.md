# Configuration and accounts

[Project overview](../README.md) · [Installation](installation.md) · [Updating](updating.md)

## State directory

All commands sharing accounts must use the same private state directory:

1. `--state-dir PATH` (or `LLM_PROXY_STATE_DIR`).
2. Otherwise `$XDG_STATE_HOME/llm-proxy`, when set.
3. Otherwise `~/.local/state/llm-proxy`.

The configuration file is `<state-dir>/config.toml`. Docker uses
`/var/lib/llm-proxy/state/config.toml` in the persistent volume. A server using a
custom state directory will not see accounts added to a different one.

For an explicit native location:

```sh
llm-proxy --state-dir "$HOME/.local/state/llm-proxy" accounts status
```

Start with [config.example.toml](../config.example.toml); review and merge it into
`<state-dir>/config.toml` rather than overwriting existing settings. The server needs
a default model in config or `serve --default-model MODEL_ID`. Offline account status
does not require a valid model/config, credentials or network access.

Restart the server after config edits. Account additions/removals are visible without
restart. `serve --default-model`, `--listen-address` and `--listen-port` override the
corresponding configuration values for that run.

## Model routing

A basic Codex configuration:

```toml
default_provider = "codex"
default_model = "gpt-6.1-sol"
listen_address = "127.0.0.1"
listen_port = 18765
log_level = "info"
```

A request's model is recognized, aliases are resolved, then one canonical provider/model
route is frozen before account selection. Provider choice uses this precedence:

1. An explicit `auto_review.provider` for a recognized action-review request.
2. The effective model's configured `provider` policy (exact model, then applicable
   base-model policy for service-tier suffixes).
3. `default_provider`.

Normal conversations do not use `auto_review` overrides. Unknown, unregistered models
are rejected rather than guessed. Unmapped generic Claude-family names such as
`sonnet`, `opus` or `claude-sonnet-*` use `default_model`; explicit aliases/native model
registrations take precedence. The `[1m]` client suffix is accepted by model recognition
but does not grant an upstream context window.

### Mixed providers

```toml
default_provider = "codex"
default_model = "gpt-6.1-sol"

[aliases]
sonnet = "claude-sonnet-4-6"

[models."claude-sonnet-4-6"]
provider = "agy"
```

Both `sonnet` and the explicitly registered `claude-sonnet-4-6` select AGY. Unmapped
client aliases continue using the default Codex route. Sign in to each provider you
route to; an account for one provider cannot satisfy the other's route.

Model examples in this repository are not entitlement checks. Use canonical IDs your
provider/account actually serves; AGY IDs are not necessarily first-party API IDs.
Register additional models through `[models."MODEL_ID"]`. `GET /v1/models` lists the
configured catalog, not a live inventory of usable upstream models.

Failover keeps the selected provider, canonical model and execution options unchanged.
It never automatically moves AGY requests to Codex, or vice versa.

### Codex execution options

```toml
[models."gpt-6.1-sol"]
responses_lite = false
# search_model = "another-account-supported-search-model"
```

The GPT-5.6/GPT-6 families default to Responses Lite. An explicit full-lane policy can
allow parallel tools where supported; client restrictions still apply. Codex
`-fast`/`-ultrafast` suffixes select priority/ultrafast service tiers after the final
provider has been selected. AGY native IDs are not rewritten using these Codex tiers.

### Antigravity

Native browser sign-in does not require a manually supplied OAuth client pair:

```sh
llm-proxy accounts add agy --name google-personal
llm-proxy accounts status agy
```

The localhost callback is `http://localhost:51121/oauth-callback`. AGY has no device-code
login or automatic import of credentials from the installed Antigravity application.
Login can onboard a managed project when necessary; discovery failure is an error,
not a guessed project ID. Keep callback listeners on loopback. The shipped Docker
configuration does not publish this browser callback.

An uncached login/refresh acquires public OAuth client configuration from an
integrity-pinned reference and caches it privately in the selected state directory.
Complete `LLM_PROXY_AGY_CLIENT_ID` and `LLM_PROXY_AGY_CLIENT_SECRET` environment overrides
are optional; partial or empty overrides fail. Do not put client secrets in TOML, Git
or client settings. See [the source, hash and trust boundary](antigravity.md#provenance).

AGY supports only the documented subset. In particular, **omit reasoning effort**:
explicit `output_config.effort` or `reasoning_effort` returns
`unsupported_reasoning_effort` before account selection/dispatch, including explicit
`none` or `medium`. Omission retains the configured thinking policy for Claude
`-thinking` models; no unverified effort-to-budget mapping is invented. Clients that
automatically send effort need to omit it or choose a supporting provider.

Also unsupported: Gemini function tools, structured output, hosted search, compaction,
forced tool choice, live usage and banked quota resets. Rich function schemas use
`parametersJsonSchema`; acceptance by the internal service has not been verified live.
See [all AGY limits](antigravity.md#current-limits-and-operational-notes).

## Client configuration example

The inbound API is Anthropic Messages-compatible, not a universal coding-agent API.
The following is a **Claude Code-specific example**. For other clients, use their
base-URL settings and verify the protocol/features they send.

Merge these keys into `env` in `~/.claude/settings.json`, preserving other settings:

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:18765",
    "ANTHROPIC_AUTH_TOKEN": "local",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
    "CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK": "1",
    "CLAUDE_CODE_AUTO_MODE_SERVER": "0"
  }
}
```

Use your published host port if different. Start a new client session after changing
its environment. These client variables belong to the coding agent, not the proxy
process. `ANTHROPIC_AUTH_TOKEN=local` is a placeholder for client configuration:
**the proxy does not authenticate incoming clients**, and does not forward that value
as a provider credential.

### Action-review / Auto mode routing

For the recognized client action-review request, provider and model can be configured
independently of conversations. For example, with AGY as the conversation default:

```toml
default_provider = "agy"
default_model = "claude-sonnet-4-6"

[auto_review]
provider = "codex"
model = "gpt-6-luna"
```

This requires a separate Codex account. AGY does not support the structured-output
requests used by the documented classifier workflow; do not route that classifier to
AGY. Review errors remain errors and never become synthetic approvals.

`auto_review.model` defaults to `gpt-6-luna`; its provider otherwise follows model/default
routing. Keep the example review model within your account's entitlement. The
client-specific `CLAUDE_CODE_AUTO_MODE_SERVER=0` selects the client-side classifier;
it does not disable permission checks. Changing a client model/configuration does
not prove the resulting classifier is safe.

## Accounts and offline status

```sh
llm-proxy accounts add codex --name personal --login-method device-code
llm-proxy accounts add codex --name work              # browser login
llm-proxy accounts status                            # cached, all existing accounts
llm-proxy accounts status codex                      # exact provider filter
llm-proxy accounts status --output-format json       # public DTO, not credentials
llm-proxy accounts status codex --refresh             # explicit live usage read
llm-proxy accounts remove ACCOUNT_ID_OR_PREFIX
```

Repeat `add` to sign in another account or renew an existing identity. Browser sign-in
may need a private window to choose another account. Active placement is provider-scoped
and sticky; quota recovery does not displace a healthy active account.

`status` is offline unless `--refresh` is explicit. It shows observation age/source;
cached data may be stale. `unknown` and `unsupported` are not reported zeroes. Provider
quota windows can differ: no universal five-hour/weekly window is assumed. If refresh
fails, cached data is still printed with the error and the command returns nonzero.
AGY live refresh currently reports unsupported rather than fabricated usage.

`remove` accepts a full UUID or a unique prefix of at least eight characters; ambiguous
prefixes require more characters. Quota-reset commands require a full account UUID.
Removal unpublishes the account but does not revoke the provider's OAuth grant.

For Docker, prefix supported commands with `docker compose exec proxy` and retain the
same project/volume; do not use browser login through an unpublished callback.

## Quota reset and uncertain outcomes

Three different concepts must not be confused:

- **Scheduled reset:** the provider's ordinary window recovery time.
- **Prepaid credits:** a separate balance/spend entitlement when reported.
- **Banked resets:** accumulated rights that an explicit reset redemption consumes.

Codex banked-reset commands, where the backend/account supports them:

```sh
llm-proxy accounts reset-quota codex ACCOUNT_ID             # preview only
llm-proxy accounts reset-quota codex ACCOUNT_ID --yes       # consume a banked reset
```

This does not buy prepaid credits or merely wait for scheduled recovery. There is
no automatic redemption or background usage polling. If a reset happened outside
the proxy, run `accounts status codex --refresh`: authoritative availability may clear
the captured local quota state without clearing `NeedsLogin`, changing the generation
or taking over the active placement. Missing/false data or percentages alone do not
prove recovery. Generic 429/capacity/model/network/5xx failures do not prove exhaustion.

A timeout after dispatch may mean the reset was consumed. Keep the reported request
ID; do not start a new logical request. Only when deliberately reconciling/retrying
the same operation, use:

```sh
llm-proxy accounts reset-quota codex ACCOUNT_ID --yes --retry REQUEST_ID
```

Mutations are not automatically retried. Backend idempotency retention is not
promised; follow-up or uncertain outcomes return nonzero even when a redemption may
have succeeded. AGY banked resets are unsupported.

## Legacy commands

`accounts login` still signs in to Codex; `accounts list` retains its legacy output.
Top-level `login`, `status` and `reset-quota` shortcuts remain available but hidden
from normal help. Prefer the grouped `accounts` commands in new scripts.
