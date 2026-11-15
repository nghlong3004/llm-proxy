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
