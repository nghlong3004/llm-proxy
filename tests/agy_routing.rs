//! Explicit native model registration and mixed-provider routing; no live traffic.
use llm_proxy::{
    adapters::inbound::http::anthropic::Anthropic,
    application::request_routing::ConfiguredRequestRoutingPolicy,
    config::Config,
    domain::RequestPurpose,
    ports::{protocol_adapter::ProtocolAdapter, request_routing::RequestRoutingPolicy},
};
use serde_json::json;
use std::sync::Arc;

fn config(text: &str) -> Config {
    let config: Config = toml::from_str(text).unwrap();
    config.validate().unwrap();
    config
}

#[test]
fn native_claude_registration_wins_over_legacy_claude_fallback() {
    let config = config(
        "default_model = 'gpt-6.1-sol'\n\
         [models.'claude-sonnet-4-6']\nprovider = 'agy'\n\
         [aliases]\nsonnet = 'claude-sonnet-4-6'\n",
    );
    assert_eq!(
        config
            .resolve_route("claude-sonnet-4-6[1m]", None)
            .unwrap()
            .model,
        "claude-sonnet-4-6"
    );
    assert_eq!(
        config.resolve_route("opus", None).unwrap().model,
        "gpt-6.1-sol"
    );
    let policy = ConfiguredRequestRoutingPolicy::new(Arc::new(config));
    let request = Anthropic
        .decode(json!({
            "model":"sonnet", "messages":[{"role":"user","content":"synthetic"}]
        }))
        .unwrap();
    let route = policy.route(&request).unwrap();
    assert_eq!(route.provider, "agy");
    assert_eq!(route.model, "claude-sonnet-4-6");
}

#[test]
fn explicit_review_route_overrides_model_policy_without_affecting_conversation() {
    let config = config(
        "default_provider = 'agy'\ndefault_model = 'claude-sonnet-4-6'\n\
         [auto_review]\nprovider = 'codex'\nmodel = 'gpt-6-luna'\n\
         [models.'claude-sonnet-4-6']\nprovider = 'agy'\n",
    );
    let policy = ConfiguredRequestRoutingPolicy::new(Arc::new(config));
    let mut request = Anthropic
        .decode(json!({
            "model":"sonnet", "messages":[{"role":"user","content":"synthetic"}]
        }))
        .unwrap();
    assert_eq!(policy.route(&request).unwrap().provider, "agy");
    request.purpose = RequestPurpose::ActionReview;
    let review = policy.route(&request).unwrap();
    assert_eq!(review.provider, "codex");
    assert_eq!(review.model, "gpt-6-luna");
}

#[test]
fn review_without_provider_override_uses_its_own_model_policy() {
    let config = config(
        "default_model = 'gpt-6.1-sol'\n\
         [auto_review]\nmodel = 'claude-sonnet-4-6'\n\
         [models.'claude-sonnet-4-6']\nprovider = 'agy'\n",
    );
    let policy = ConfiguredRequestRoutingPolicy::new(Arc::new(config));
    let mut request = Anthropic
        .decode(json!({
            "model":"opus", "messages":[{"role":"user","content":"synthetic"}]
        }))
        .unwrap();
    assert_eq!(policy.route(&request).unwrap().provider, "codex");
    request.purpose = RequestPurpose::ActionReview;
    assert_eq!(policy.route(&request).unwrap().provider, "agy");
}

#[test]
fn provider_specific_suffixes_are_not_treated_as_codex_service_tiers() {
    let config = config(
        "default_model = 'gpt-6.1-sol'\n\
         [models.'native-fast']\nprovider = 'agy'\n\
         [models.'gpt-5.5']\nprovider = 'codex'\n",
    );
    let agy = config.resolve_route("native-fast", None).unwrap();
    assert_eq!(agy.model, "native-fast");
    assert_eq!(
        agy.options,
        llm_proxy::ports::request_routing::RouteOptions::Standard
    );
    assert!(matches!(
        config.resolve_route("gpt-5.5-fast", None).unwrap().options,
        llm_proxy::ports::request_routing::RouteOptions::Codex(
            llm_proxy::ports::request_routing::CodexOptions {
                service_tier: Some(llm_proxy::ports::request_routing::ServiceTier::Priority),
                ..
            }
        )
    ));
}

#[test]
fn default_native_model_is_not_remapped_to_another_claude_model() {
    let config = config(
        "default_provider = 'agy'\ndefault_model = 'claude-sonnet-4-6'\n\
         [auto_review]\nmodel = 'sonnet'\n",
    );
    assert_eq!(
        config
            .resolve_route("claude-sonnet-4-6", None)
            .unwrap()
            .model,
        "claude-sonnet-4-6"
    );
    assert_eq!(
        config.resolve_route("sonnet", None).unwrap().model,
        "claude-sonnet-4-6"
    );
    assert!(config.resolve_route("gemini-not-registered", None).is_err());
}

#[test]
fn exact_codex_lane_policy_overrides_a_native_default_provider() {
    let config = config(
        "default_provider = 'agy'\ndefault_model = 'claude-sonnet-4-6'\n\
         [auto_review]\nmodel = 'sonnet'\n\
         [models.'gpt-5.5-fast']\nprovider = 'codex'\n",
    );
    let policy = ConfiguredRequestRoutingPolicy::new(Arc::new(config));
    let request = Anthropic
        .decode(json!({
            "model":"gpt-5.5-fast", "messages":[{"role":"user","content":"synthetic"}]
        }))
        .unwrap();
    assert_eq!(policy.route(&request).unwrap().provider, "codex");
}

#[test]
fn invalid_provider_configuration_and_unbounded_output_are_rejected() {
    for text in [
        "default_model='gpt-5.5'\ndefault_provider=' '",
        "default_model='gpt-5.5'\n[models.native]\nprovider=' '",
        "default_model='gpt-5.5'\nagy_max_output_tokens=65537",
        "default_model='gpt-5.5'\nagy_thinking_budget=40960",
        "default_model='gpt-5.5'\nagy_endpoint='http://external.invalid'",
    ] {
        assert!(toml::from_str::<Config>(text).unwrap().validate().is_err());
    }
}

#[test]
fn review_override_normalizes_codex_lanes_after_alias_recognition_with_native_default() {
    use llm_proxy::ports::request_routing::{RouteOptions, ServiceTier};
    let config = config(
        "default_provider='agy'\ndefault_model='claude-sonnet-4-6'\n[auto_review]\nprovider='codex'\nmodel='gpt-5.5-fast'\n[aliases]\nquick='gpt-5.5-ultrafast'\n",
    );
    for (name, tier) in [
        ("gpt-5.5-fast", ServiceTier::Priority),
        ("quick", ServiceTier::Ultrafast),
    ] {
        config.recognize_model(name).unwrap();
        let route = config.resolve_route(name, Some("codex")).unwrap();
        assert_eq!(route.provider, "codex");
        assert_eq!(route.model, "gpt-5.5");
        assert!(
            matches!(route.options, RouteOptions::Codex(options) if options.service_tier == Some(tier) && !options.responses_lite && options.search_model == "gpt-5.5")
        );
    }
    assert_eq!(
        config.resolve_route("gpt-5.5-fast", None).unwrap().model,
        "gpt-5.5-fast"
    );
}
