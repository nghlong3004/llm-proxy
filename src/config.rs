//! Configuration and explicit model routing, independent of account selection.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
};

pub const CODEX_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
pub const CODEX_ACCOUNT_ENDPOINT: &str = "https://chatgpt.com/backend-api";
pub const AGY_ENDPOINT: &str = "https://daily-cloudcode-pa.googleapis.com";
pub const BUILTIN_MODELS: &[&str] = &[
    "gpt-5.2",
    "gpt-5.3-codex",
    "gpt-5.3-codex-spark",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.5",
    "gpt-5.6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-6-astra",
    "gpt-6-luna",
    "gpt-6-sol",
    "gpt-6.1-sol",
    "gpt-6.1-terra",
];
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub default_model: Option<String>,
    #[serde(alias = "default_backend")]
    pub default_provider: String,
    pub aliases: BTreeMap<String, String>,
    pub models: BTreeMap<String, ModelPolicy>,
    pub listen_address: IpAddr,
    #[serde(alias = "port")]
    pub listen_port: u16,
    pub codex_endpoint: String,
    pub codex_account_endpoint: String,
    pub agy_endpoint: String,
    pub agy_max_output_tokens: u32,
    pub agy_thinking_budget: u32,
    pub log_level: String,
    pub auto_review: AutoReviewConfig,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            default_model: None,
            default_provider: "codex".into(),
            aliases: BTreeMap::new(),
            models: BTreeMap::new(),
            listen_address: Ipv4Addr::LOCALHOST.into(),
            listen_port: 18765,
            codex_endpoint: CODEX_ENDPOINT.into(),
            codex_account_endpoint: CODEX_ACCOUNT_ENDPOINT.into(),
            agy_endpoint: AGY_ENDPOINT.into(),
            agy_max_output_tokens: 40960,
            agy_thinking_budget: 32000,
            log_level: "info".into(),
            auto_review: AutoReviewConfig::default(),
        }
    }
}
/// Action-review routing defaults to Luna; the provider inherits the default route.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoReviewConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
}
impl Default for AutoReviewConfig {
    fn default() -> Self {
        Self {
            provider: None,
            model: Some("gpt-6-luna".into()),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelPolicy {
    /// Outbound provider for this canonical model; otherwise inherits default_provider.
    pub provider: Option<String>,
    pub responses_lite: Option<bool>,
    pub search_model: Option<String>,
}
#[derive(Clone, Debug)]
pub struct ResolvedModel {
    pub id: String,
    pub provider: String,
    pub responses_lite: bool,
    pub service_tier: Option<String>,
    pub search_model: String,
}
impl Config {
    pub fn load(directory: &Path) -> Result<Self> {
        let path = directory.join("config.toml");
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).context("Invalid config.toml"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.log_level
            .parse::<tracing::level_filters::LevelFilter>()
            .context("Invalid log_level; use off, error, warn, info, debug or trace")?;
        let model = self
            .default_model
            .as_deref()
            .context("Set default_model in config.toml or pass serve --default-model MODEL")?;
        if model.trim().is_empty() {
            bail!("Default model cannot be empty");
        }
        validate_endpoint(&self.codex_endpoint)?;
        validate_endpoint(&self.codex_account_endpoint)?;
        validate_endpoint(&self.agy_endpoint)?;
        if self.default_provider.trim().is_empty() {
            bail!("default_provider cannot be empty");
        }
        if !(1..=65536).contains(&self.agy_max_output_tokens)
            || self.agy_thinking_budget >= self.agy_max_output_tokens
        {
            bail!("agy_max_output_tokens must be 1–65536 and greater than agy_thinking_budget");
        }
        for policy in self.models.values() {
            if policy
                .provider
                .as_ref()
                .is_some_and(|provider| provider.trim().is_empty())
            {
                bail!("Model provider cannot be empty");
            }
        }
        if self.listen_port == 0 {
            bail!("Port must be between 1 and 65535");
        }
        for target in self.aliases.values() {
            if target.trim().is_empty() {
                bail!("Model aliases cannot have empty targets");
            }
        }
        if let Some(provider) = &self.auto_review.provider
            && provider.trim().is_empty()
        {
            bail!("auto_review.provider cannot be empty");
        }
        if let Some(model) = &self.auto_review.model {
            if model.trim().is_empty() {
                bail!("auto_review.model cannot be empty");
            }
            self.resolve(model).context("Invalid auto_review.model")?;
        }
        Ok(())
    }
    pub fn resolve(&self, requested: &str) -> Result<ResolvedModel> {
        let requested = requested.strip_suffix("[1m]").unwrap_or(requested);
        let target = if let Some(target) = self.aliases.get(requested) {
            target.as_str()
        } else if self.models.contains_key(requested)
            || self.default_model.as_deref() == Some(requested)
        {
            // An explicitly registered native model wins over the generic Claude alias.
            requested
        } else if is_claude_alias(requested) {
            self.default_model
                .as_deref()
                .context("No default model configured")?
        } else if self.catalog().contains(requested)
            || self.catalog().contains(
                requested
                    .strip_suffix("-ultrafast")
                    .or_else(|| requested.strip_suffix("-fast"))
                    .unwrap_or(requested),
            )
        {
            requested
        } else {
            bail!("Unknown model '{requested}'; use GET /v1/models or configure a model alias");
        };
        let provider = self
            .models
            .get(target)
            .or_else(|| {
                let base = target
                    .strip_suffix("-ultrafast")
                    .or_else(|| target.strip_suffix("-fast"))?;
                self.models.get(base)
            })
            .and_then(|policy| policy.provider.as_deref())
            .unwrap_or(&self.default_provider);
        let (id, tier) = if provider != "codex" {
            (target, None)
        } else if let Some(id) = target.strip_suffix("-ultrafast") {
            (id, Some("ultrafast"))
        } else if let Some(id) = target.strip_suffix("-fast") {
            (id, Some("priority"))
        } else {
            (target, None)
        };
        let policy = self.models.get(id).cloned().unwrap_or_default();
        let responses_lite = policy
            .responses_lite
            .unwrap_or_else(|| id.starts_with("gpt-5.6-") || id.starts_with("gpt-6"));
        let search_model = policy
            .search_model
            .unwrap_or_else(|| id.replace("-luna", "-sol"));
        Ok(ResolvedModel {
            id: id.into(),
            provider: provider.into(),
            responses_lite,
            service_tier: tier.map(str::to_owned),
            search_model,
        })
    }
    pub fn catalog(&self) -> BTreeSet<String> {
        let mut models: BTreeSet<String> = BUILTIN_MODELS.iter().map(|s| (*s).into()).collect();
        models.extend(self.default_model.iter().cloned());
        models.extend(self.aliases.keys().cloned());
        models.extend(self.aliases.values().cloned());
        models.extend(self.models.keys().cloned());
        models.extend(["haiku", "sonnet", "opus", "fable"].map(str::to_owned));
        models
    }
}
fn is_claude_alias(id: &str) -> bool {
    ["haiku", "sonnet", "opus", "fable"]
        .iter()
        .any(|family| id == *family || id.starts_with(&format!("claude-{family}-")))
}
pub fn state_dir() -> Result<PathBuf> {
    if let Some(state) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(state).join("llm-proxy"));
    }
    Ok(
        PathBuf::from(std::env::var_os("HOME").context("HOME is not set; pass --state-dir")?)
            .join(".local/state/llm-proxy"),
    )
}
pub fn validate_endpoint(endpoint: &str) -> Result<()> {
    let url = url::Url::parse(endpoint).context("Invalid upstream URL")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("Upstream URL cannot contain credentials, query or fragment");
    }
    let loopback = url
        .host_str()
        .is_some_and(|s| s == "localhost" || s == "127.0.0.1" || s == "[::1]");
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        bail!("Upstream URL must use HTTPS (HTTP is only permitted on loopback)");
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_use_selected_default_and_unknown_models_do_not_fall_back() {
        let c = Config {
            default_model: Some("gpt-6.1-sol".into()),
            ..Config::default()
        };
        assert_eq!(
            c.resolve("claude-sonnet-4-6[1m]").unwrap().id,
            "gpt-6.1-sol"
        );
        assert!(c.resolve("made-up-model").is_err());
        assert_eq!(c.resolve("gpt-5.4").unwrap().id, "gpt-5.4");
    }
    #[test]
    fn custom_models_and_lane_policy_are_explicit() {
        let c: Config = toml::from_str("default_model = 'custom-codex'\n[models.custom-codex]\nresponses_lite = false\n[aliases]\nsmall = 'custom-codex'\n").unwrap();
        assert_eq!(c.resolve("small").unwrap().id, "custom-codex");
        assert!(!c.resolve("custom-codex").unwrap().responses_lite);
    }
    #[test]
    fn listen_address_defaults_to_loopback_and_can_be_explicitly_set_for_containers() {
        let default = serde_json::to_value(Config::default()).unwrap();
        assert_eq!(default["listen_address"], "127.0.0.1");
        let config: Config =
            toml::from_str("default_model = 'gpt-6.1-sol'\nlisten_address = '0.0.0.0'\n").unwrap();
        config.validate().unwrap();
        assert_eq!(
            serde_json::to_value(config).unwrap()["listen_address"],
            "0.0.0.0"
        );
        assert!(toml::from_str::<Config>("listen_address = 'not-an-ip'").is_err());
    }
}
