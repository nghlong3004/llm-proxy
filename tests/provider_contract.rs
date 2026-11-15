//! Golden requests produced by the real upstream translator, not by our mock.
use llm_proxy::{
    accounts::Credential,
    adapters::inbound::http::anthropic::Anthropic,
    adapters::outbound::codex::{client::headers, reasoning::ReasoningSigner, request::translate},
    config::Config,
    domain::RequestContext,
    ports::protocol_adapter::ProtocolAdapter,
};
use serde_json::{Value, json};

fn fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/codex/raine-contract.json")).unwrap()
}
fn compare_case(case: &Value) {
    let request = Anthropic.decode(case["input"].clone()).unwrap();
    let config = Config {
        default_model: Some(case["model"].as_str().unwrap().into()),
        ..Default::default()
    };
    let actual = translate(
        &request,
        &config.resolve("sonnet").unwrap(),
        "mock-account",
        "mock-generation",
        &ReasoningSigner::new([0; 32]),
        Some("contract-session"),
    )
    .unwrap();
    assert_eq!(
        actual, case["expected"],
        "upstream contract: {}",
        case["name"]
    );
}
#[test]
fn lite_payload_matches_raine_translator_for_medium_and_high_effort() {
    for case in fixtures().iter().filter(|case| {
        case["lite"] == true && case["name"].as_str().unwrap().ends_with("tool-schema")
    }) {
        compare_case(case);
    }
}
#[test]
fn full_payload_matches_raine_translator_for_medium_and_high_effort() {
    for case in fixtures().iter().filter(|case| {
        case["lite"] == false && case["name"].as_str().unwrap().ends_with("tool-schema")
    }) {
        compare_case(case);
    }
}
#[test]
fn structured_output_matches_raine_schema_normalization_in_both_lanes() {
    for case in fixtures().iter().filter(|case| {
        case["name"]
            .as_str()
            .unwrap()
            .ends_with("structured-output")
    }) {
        compare_case(case);
    }
}
#[test]
fn codex_protocol_headers_match_raine_with_local_full_lane_branding() {
    let credential = Credential {
        access_token: "mock-access".into(),
        refresh_token: "mock-refresh".into(),
        id_token: None,
        expires_at: u64::MAX,
        subject: "mock-user".into(),
        account_id: "mock-workspace".into(),
        generation: "mock-generation".into(),
    };
    let context = RequestContext {
        request_id: "contract-request".into(),
        session_id: Some("contract-session".into()),
    };
    for lite in [true, false] {
        let mut expected = fixtures()
            .into_iter()
            .find(|case| case["lite"] == lite)
            .unwrap()["expected_headers"]
            .clone();
        // Full lane identifies this application; Lite must use Codex's originator.
        if !lite {
            expected["originator"] = json!("llm-proxy");
            expected["user-agent"] = json!(format!("llm-proxy/{}", env!("CARGO_PKG_VERSION")));
        }
        let actual = headers(&credential, &context, lite, false)
            .unwrap()
            .iter()
            .map(|(name, value)| (name.as_str().to_owned(), json!(value.to_str().unwrap())))
            .collect::<serde_json::Map<_, _>>();
        assert_eq!(Value::Object(actual), expected, "headers: lite={lite}");
    }
}
