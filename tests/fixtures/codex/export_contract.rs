use claude_code_proxy::providers::codex::{
    auth::token_store::StoredAuth,
    client::build_codex_headers,
    translate::request::{TranslateOptions, translate_request},
};
use claude_code_proxy::{MessagesRequest, RequestContext};
use serde_json::{Value, json};
fn main() -> anyhow::Result<()> {
    let cases: Vec<Value> = serde_json::from_reader(std::io::stdin())?;
    let mut outputs = Vec::new();
    for case in cases {
        let input = case["input"].clone();
        let request: MessagesRequest = serde_json::from_value(input.clone())?;
        let lite = case["lite"].as_bool().unwrap_or(false);
        let output = translate_request(
            &request,
            TranslateOptions {
                model: case["model"].as_str().unwrap().to_owned(),
                use_responses_lite: lite,
                service_tier: None,
                session_id: Some("contract-session".into()),
            },
        )?;
        let auth = StoredAuth {
            access: "mock-access".into(),
            refresh: "mock-refresh".into(),
            expires: u64::MAX,
            account_id: Some("mock-workspace".into()),
        };
        let ctx = RequestContext {
            req_id: "contract-request".into(),
            session_id: Some("contract-session".into()),
            session_seq: None,
            provider: "codex".into(),
            traffic: None,
            monitor: None,
        };
        let headers = build_codex_headers(&auth, &ctx, lite)
            .map_err(|_| anyhow::anyhow!("reference header construction failed"))?;
        let headers = headers
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), json!(v.to_str().unwrap())))
            .collect::<serde_json::Map<_, _>>();
        outputs.push(json!({"name":case["name"],"model":case["model"],"lite":lite,"input":input,"expected":output,"expected_headers":headers}));
    }
    println!("{}", serde_json::to_string_pretty(&outputs)?);
    Ok(())
}
