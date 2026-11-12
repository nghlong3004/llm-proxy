//! Configuration and explicit model routing, independent of account selection.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub const CODEX_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
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
    #[serde(alias = "port")]
    pub listen_port: u16,
    pub codex_endpoint: String,
    pub log_level: String,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            default_model: None,
            default_provider: "codex".into(),
            aliases: BTreeMap::new(),
            models: BTreeMap::new(),
            listen_port: 18765,
            codex_endpoint: CODEX_ENDPOINT.into(),
            log_level: "info".into(),
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelPolicy {
    pub responses_lite: Option<bool>,
    pub search_model: Option<String>,
}
#[derive(Clone, Debug)]
pub struct ResolvedModel {
    pub id: String,
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
        if self.listen_port == 0 {
            bail!("Port must be between 1 and 65535");
        }
        for target in self.aliases.values() {
            if target.trim().is_empty() {
                bail!("Model aliases cannot have empty targets");
            }
        }
        Ok(())
    }
    pub fn resolve(&self, requested: &str) -> Result<ResolvedModel> {
        let requested = requested.strip_suffix("[1m]").unwrap_or(requested);
        let target = if let Some(target) = self.aliases.get(requested) {
            target.as_str()
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
        let (id, tier) = if let Some(id) = target.strip_suffix("-ultrafast") {
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
}
