# Quy ước

## Tên và CLI

- Module theo trách nhiệm, danh từ tiếng Anh `snake_case`; không gom vào
  `core/common/utils`. Bản đồ module chỉ duy trì tại [architecture](architecture.md).
- CLI `resource action`, flag `kebab-case`, key TOML `snake_case`, giá trị hữu hạn
  dùng enum. Account có `provider`, `name`; TOML có `default_provider`,
  `default_model`, `listen_address`, `listen_port`, `log_level`.
- Aliases đọc state/config cũ: `backend`, `label`, `default_backend`, `port`;
  serialize/docs dùng tên mới. Chưa phát hành nên không giữ flag CLI cũ.
- Wire URL/field upstream giữ spelling gốc, không đổi theo tên module Rust.

```text
llm-proxy --state-dir PATH accounts login --name NAME --login-method browser
llm-proxy accounts login --name NAME --login-method device-code
llm-proxy accounts list --output-format json
llm-proxy accounts remove ACCOUNT_ID
llm-proxy serve --default-model MODEL --listen-address IP --listen-port PORT
```

`--state-dir` global, env `LLM_PROXY_STATE_DIR`, chứa config/accounts/credentials/logs.
`listen_address` là IP, native default loopback; wildcard chỉ explicit trong container.
`[auto_review]` mặc định model Luna, provider kế thừa default; explicit model/alias
vẫn override. Client `CLAUDE_CODE_AUTO_MODE_SERVER=0` không phải server config.

## Nguồn tham khảo

`domain` là contract project tự thiết kế, không phải “core” chép từ reference.
`raine/claude-code-proxy` tham khảo OAuth/native headers/Responses lanes/translation;
`kapybara-org/zerosub` tham khảo identity/quota/account-bound reasoning, không port
source ZeroSub. Không mang agent runtime vào proxy. Revision source port nằm trong
`src/providers/codex/schema.rs` và [fixture provenance](../tests/fixtures/codex/README.md),
MIT attribution giữ ở [LICENSE](../LICENSE), cũng ship trong runtime image.
