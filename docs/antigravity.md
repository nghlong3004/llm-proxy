# Antigravity outbound reference

## Provenance

The native `agy` adapter is an independent Rust integration using protocol reference
material from [badrisnarayanan/antigravity-claude-proxy](https://github.com/badrisnarayanan/antigravity-claude-proxy/tree/7c672fdc6fa62fcd50e8f75df5644dad6a63c2dc),
pinned to commit `7c672fdc6fa62fcd50e8f75df5644dad6a63c2dc` (2026-11-07).
The reference is MIT licensed, copyright (c) 2024 Badri Narayanan S; its attribution
is retained in [LICENSE](../LICENSE). Upstream `src/constants.js` additionally credits
[NoeFabris/opencode-antigravity-auth](https://github.com/NoeFabris/opencode-antigravity-auth)
for protocol work. No upstream JavaScript is executed or bundled.

Source paths at the pinned revision:

- `src/constants.js`, `src/auth/oauth.js`: installed-client OAuth configuration,
  Google token/userinfo endpoints and PKCE browser flow.
- `src/account-manager/credentials.js`, `src/account-manager/onboarding.js`:
  `loadCodeAssist`, managed project response shapes and explicit login onboarding.
- `src/cloudcode/request-builder.js`, `src/format/content-converter.js`:
  Cloud Code Assist request wrapper and native message/tool formats.
- `src/cloudcode/sse-streamer.js`, `src/format/response-converter.js`:
  candidate parts, opaque thought signatures, usage and finish metadata.
- `src/cloudcode/rate-limit-parser.js`, `src/cloudcode/model-api.js`:
  ambiguous capacity/rate errors and account-dependent canonical model IDs.

Tests use fabricated credentials, project/user/model identities and source-derived
JSON contracts only. They do not capture live prompts or credentials. Provider client
values are not embedded in source/tests or disguised through encoding. By default,
login/refresh lazily downloads the public reference's `src/constants.js` from the
pinned raw GitHub URL and verifies SHA-256
`abebb46f041f9daa26bba4d855626946bad33edd369da1dbfe1bad4d0bb9b7d8` before extraction.
Only the two string fields `OAUTH_CONFIG.clientId/clientSecret` are read. JavaScript is
never executed, and source-provided endpoints/prompts/configuration are not adopted.

The verified bytes are atomically cached at
`<state-dir>/agy-oauth-client/7c672fdc6fa62fcd50e8f75df5644dad6a63c2dc/reference.txt`.
Directories are private (0700), files are 0600, and cross-process locks protect cache
reads/publication without spanning network awaits. Cache loads recheck the digest;
corruption errors rather than silently redownloading or accepting a different source.
The cache has no account tokens and is distinct from account credential storage.

A complete `LLM_PROXY_AGY_CLIENT_ID` / `LLM_PROXY_AGY_CLIENT_SECRET` pair overrides
reference acquisition; partial, empty or malformed overrides are errors, not mixed
with reference values. When overriding, configure the same pair for login and refresh.
Explicit synthetic test endpoint injection never enables reference downloads implicitly.
Neither provider construction, offline status nor Codex-only startup acquires the
reference. First uncached AGY login/refresh needs access to raw.githubusercontent.com;
there is no branch-head update, redirect following or integrity bypass. The client
must still be authorized/accepted by the service; this public reference and arbitrary
Google OAuth app registration do not establish Antigravity entitlement.

## Login project protocol

Project discovery/onboarding additionally follows
[NoeFabris/opencode-antigravity-auth](https://github.com/NoeFabris/opencode-antigravity-auth/tree/821f824440b2b7c9ffe96bbdfad9d5cdead2f1de),
pinned to `821f824440b2b7c9ffe96bbdfad9d5cdead2f1de` (MIT, copyright (c) 2025 Jens;
attribution retained in [LICENSE](../LICENSE)):

- [`src/antigravity/oauth.ts`](https://github.com/NoeFabris/opencode-antigravity-auth/blob/821f824440b2b7c9ffe96bbdfad9d5cdead2f1de/src/antigravity/oauth.ts):
  `loadManagedProject` discovery contract.
- [`src/plugin/project.ts`](https://github.com/NoeFabris/opencode-antigravity-auth/blob/821f824440b2b7c9ffe96bbdfad9d5cdead2f1de/src/plugin/project.ts):
  discovery/onboarding metadata, tier selection, response shapes and pending polling.
- [`src/constants.ts`](https://github.com/NoeFabris/opencode-antigravity-auth/blob/821f824440b2b7c9ffe96bbdfad9d5cdead2f1de/src/constants.ts):
  daily endpoint, fallback version/changelist and endpoint-specific User-Agents.

The llm-proxy baseline `7c652b2cbd2a74557faa238648ef1df0c4219e21` instead used
production Cloud Code for login, numeric metadata and `tierId`. These request-contract
differences are corrected, but are not proof of the cause of any particular account's
missing-project response. No authenticated response has been captured or inspected.

| Operation | Contract |
| --- | --- |
| Discovery | `POST https://daily-cloudcode-pa.googleapis.com/v1internal:loadCodeAssist`; `{"metadata":{"ideType":"ANTIGRAVITY"}}`; 10-second request timeout |
| Onboarding | `POST https://daily-cloudcode-pa.googleapis.com/v1internal:onboardUser`; `{"tier_id":"<selected-tier>","metadata":{"ide_type":"ANTIGRAVITY","ide_version":"2.24.0","ide_name":"antigravity"}}`; 30-second request timeout |

Both send Bearer authorization and JSON content type. Discovery uses exactly
`antigravity/hub/2.24.0 (aidev_client; os_type=darwin; arch=arm64; cl=1000335682)`.
Onboarding appends ` google-api-nodejs-client/10.3.0` to that User-Agent and adds
`Accept: */*` and `X-Goog-Api-Client: gl-node/22.21.1`. Discovery does **not** use
onboarding's Node client suffix or `X-Goog-Api-Client`. Neither sends numeric platform,
plugin metadata or `mode`. The fixed platform text follows the pinned wire reference,
not host detection. llm-proxy pins the reference's fallback version/changelist rather
than introducing its dynamic version lookup; future service acceptance is unverified.
Token/userinfo headers and verified OAuth-client acquisition retain the existing
implementation; this change does not copy the reference's separate OAuth User-Agent.

Tier selection is first default allowed tier with a usable ID → first allowed entry's
usable ID → `currentTier.id` → `free-tier`. IDs are strings, trimmed and required to be
nonempty (stricter than JavaScript truthiness). Project values likewise accept only a
nonempty trimmed string or object with such an `id`.

Onboarding remains an explicit-login mutation. Only successful, valid pending
responses (false/absent `done`) permit another onboarding POST: at most 10 total,
5 seconds between requests. HTTP failures, top-level provider errors, malformed
responses and transport/read failures stop immediately; uncertain mutations are not
replayed. These bounds follow the reference; there is no operation-name GET contract.

**Intentional robustness improvement:** `done: true` without a usable project triggers
at most three read-only discovery requests, immediately then at 5-second intervals.
Discovery errors stop the recheck; no recheck ever restarts onboarding. This assumes
provisioning may become visible after operation completion, but that behavior has not
been confirmed live. The reference does not do this recheck. Unlike its permissive
OAuth flow and later hardcoded project fallback, llm-proxy requires an actual managed
project before login succeeds or account credentials are published. Inference and
background refresh never discover or onboard projects. Codex is unchanged.

Sanitized diagnostics accompany project-operation CLI errors and tracing events:
operation, endpoint host (no URL path/query), HTTP status or `unavailable`, selected
tier, done state and project/ID field presence/type. Only `free-tier`/`standard-tier`
are printed verbatim; other tier values are `other`, preventing provider-supplied
identifiers/secrets from leaking. Values of project fields, operation names returned
by Google, account/user identifiers, credentials, OAuth codes and complete bodies are
never printed. Missing project, onboarding timeout, provider/HTTP rejection, malformed
response, body limit and transport failure remain distinct errors. Synthetic localhost
tests validate these contracts and persistence failure behavior, not entitlement or
successful provisioning of a real account.

Rebuild and explicitly retry using the same proxy state directory and account name
as the original login (replace the two placeholders):

```sh
cargo build --release --locked
./target/release/llm-proxy --state-dir '/path/to/existing/state' accounts add agy --name 'ACCOUNT_NAME'
```

This runs the local build directly; it does not install it or change running services.
If login still fails, share only the sanitized project-operation error chain, not the
browser authorization URL, callback URL, account state files or raw provider responses.
A successful synthetic check or release build does not establish live provisioning.

## Integration policy

- Provider key: `agy`. The default base is the daily Cloud Code Assist service;
  generation always uses `v1internal:streamGenerateContent?alt=sse`, even when the
  downstream client requests a buffered response. No hidden endpoint/model failover.
- Google browser OAuth uses PKCE, a state-checked loopback callback and proxy-owned
  tokens. A stable authenticated user ID plus managed project identifies an account.
  Discovery failure is an error; there is no guessed/default project. Onboarding is
  confined to an explicit login, not inference or background refresh.
- Refresh shares the existing bounded single-flight lifecycle, cross-process locking,
  atomic generation-checked rotation, cancellation independence and shutdown drain.
- Explicit `[models."canonical-id"].provider = "agy"` routes models independently of
  Codex. Registration does not prove entitlement. No implicit new-Claude-to-old-Claude
  substitution and no service-tier suffix rewriting of AGY model IDs.
- Caller prompts are not scrubbed, rewritten or augmented to impersonate another
  client. Upstream identity/billing prompt workarounds and signature-validator bypass
  sentinels are deliberately not copied. This can expose compatibility errors that
  the reference hides; such errors remain errors.
- Thought signatures must carry authenticated proxy provenance bound to the AGY
  account, login generation and canonical model before replay. Incoming unverified
  signatures and signatures from another provider/account/model are never replayed.
- Text, base64 images and supported Claude function-call schemas/results translate
  to native parts. Unsupported semantics fail before dispatch rather than being
  silently dropped. The proxy never executes tools.
- Generic 429/`RESOURCE_EXHAUSTED`, reset/retry hints or model capacity are not proof
  of account-wide quota exhaustion. The adapter does not infer account exhaustion
  from these signals. No retry after output, uncertain dispatch or transport failure.
- Offline account status never reads credentials. AGY live usage and banked resets
  are explicitly unsupported; unavailable telemetry is not zero.

## Current limits and operational notes

- Gemini function tools, hosted search, compaction, forced tool choice and structured
  output requests are unsupported. The tool schema converter accepts only a subset
  that it can preserve faithfully, with richer object-root schemas passed unchanged
  through Google's documented `parametersJsonSchema` field (mutually exclusive with
  `parameters`). Its acceptance by the internal Cloud Code service is not live-verified.
  Reference: [Google FunctionDeclaration](https://aistudio.google.com/api-reference/caching).
  Do not route Auto-mode structured classifiers to
  AGY; use an explicitly configured Codex review route or another supported setup.
- Explicit reasoning effort (`output_config.effort` or `reasoning_effort`) is not
  silently discarded. Every explicit finite effort value currently returns
  `unsupported_reasoning_effort` before account selection/credential exchange/inference;
  no unverified effort-to-budget mapping is invented. Omission is different from
  explicit `medium` or `none`: omitted effort preserves the existing `-thinking`
  model/config budget policy. Clients that automatically send effort need to omit
  it for AGY or select a provider that supports that control. Token counting remains
  independent of generation capability validation.
- Canonical IDs are explicitly configured. Dynamic model discovery is not performed;
  availability and naming can change independently of first-party API model IDs.
- Output/thinking caps in config are local policy, not verified provider limits.
  Local token estimation is not a Google tokenizer or quota measurement.
- Native callback login is documented for a local host/WSL. The shipped Docker
  publication does not expose the browser callback; device-code login is unavailable.
  Do not make listeners public to work around this.
- No authenticated live integration has been run. Internal APIs, installed-client
  acceptance, model availability and service terms may change. The upstream project
  warns of account restrictions/suspension risk and disclaims production readiness;
  an MIT license does not grant provider access or override service terms.
