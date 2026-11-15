//! Data-only, integrity-pinned acquisition of the public AGY installed-client configuration.
#[cfg(test)]
#[path = "agy_client_tests.rs"]
mod tests;
use crate::private_state::{atomic_bytes, lock_file, private_dir};
use anyhow::{Result, bail};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::OnceCell;

const REVISION: &str = "7c672fdc6fa62fcd50e8f75df5644dad6a63c2dc";
const REFERENCE_URL: &str = concat!(
    "https://raw.githubusercontent.com/badrisnarayanan/antigravity-claude-proxy/",
    "7c672fdc6fa62fcd50e8f75df5644dad6a63c2dc/src/constants.js"
);
const REFERENCE_SHA256: [u8; 32] = [
    0xab, 0xeb, 0xb4, 0x6f, 0x04, 0x1f, 0x9d, 0xaa, 0x26, 0xbb, 0xa4, 0xd8, 0x55, 0x62, 0x69, 0x46,
    0xba, 0xd3, 0x3e, 0xdd, 0x36, 0x9d, 0xa1, 0xdb, 0xfe, 0x1b, 0xad, 0x4d, 0x0b, 0xb9, 0xb7, 0xd8,
];
const MAX_REFERENCE_BYTES: usize = 128 * 1024;
const CONFIG_ERROR: &str = "Set both LLM_PROXY_AGY_CLIENT_ID and LLM_PROXY_AGY_CLIENT_SECRET, or unset both to use the pinned reference";

// Deliberately not Debug or Serialize: never format or publish the client values.
pub(super) struct OAuthClient {
    id: String,
    secret: String,
}
impl OAuthClient {
    fn new(id: String, secret: String) -> Result<Self> {
        if [&id, &secret].iter().any(|value| {
            value.trim().is_empty() || value.len() > 1024 || value.chars().any(char::is_control)
        }) {
            bail!("{CONFIG_ERROR}");
        }
        Ok(Self { id, secret })
    }
    pub(super) fn pair(&self) -> (&str, &str) {
        (&self.id, &self.secret)
    }
}

#[derive(Clone)]
struct Reference {
    url: String,
    digest: [u8; 32],
    revision: String,
}
impl Default for Reference {
    fn default() -> Self {
        Self {
            url: REFERENCE_URL.into(),
            digest: REFERENCE_SHA256,
            revision: REVISION.into(),
        }
    }
}

enum Source {
    Explicit(String, String),
    InvalidOverride,
    Reference { root: PathBuf, reference: Reference },
}

#[derive(Clone)]
pub(super) struct ClientSource {
    source: Arc<Source>,
    resolved: Arc<OnceCell<OAuthClient>>,
}
impl ClientSource {
    pub(super) fn explicit(id: String, secret: String) -> Self {
        Self {
            source: Arc::new(Source::Explicit(id, secret)),
            resolved: Arc::new(OnceCell::new()),
        }
    }

    pub(super) fn from_environment(root: PathBuf) -> Self {
        fn value(name: &str) -> Result<Option<String>, ()> {
            match std::env::var(name) {
                Ok(value) => Ok(Some(value)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(std::env::VarError::NotUnicode(_)) => Err(()),
            }
        }
        match (
            value("LLM_PROXY_AGY_CLIENT_ID"),
            value("LLM_PROXY_AGY_CLIENT_SECRET"),
        ) {
            (Ok(id), Ok(secret)) => Self::with_override(root, id, secret),
            _ => Self {
                source: Arc::new(Source::InvalidOverride),
                resolved: Arc::new(OnceCell::new()),
            },
        }
    }

    fn with_override(root: PathBuf, id: Option<String>, secret: Option<String>) -> Self {
        let source = match (id, secret) {
            (None, None) => Source::Reference {
                root,
                reference: Reference::default(),
            },
            (Some(id), Some(secret)) => Source::Explicit(id, secret),
            _ => Source::InvalidOverride,
        };
        Self {
            source: Arc::new(source),
            resolved: Arc::new(OnceCell::new()),
        }
    }

    pub(super) async fn resolve(&self, http: &reqwest::Client) -> Result<&OAuthClient> {
        self.resolved
            .get_or_try_init(|| async {
                match self.source.as_ref() {
                    Source::Explicit(id, secret) => OAuthClient::new(id.clone(), secret.clone()),
                    Source::InvalidOverride => bail!("{CONFIG_ERROR}"),
                    Source::Reference { root, reference } => {
                        load_reference(http, root, reference).await
                    }
                }
            })
            .await
    }
}

fn cache_directory(root: &Path, reference: &Reference) -> Result<PathBuf> {
    private_dir(root)?;
    let parent = root.join("agy-oauth-client");
    private_dir(&parent)?;
    let directory = parent.join(&reference.revision);
    private_dir(&directory)?;
    Ok(directory)
}

fn read_cache(directory: &Path, reference: &Reference) -> Result<Option<OAuthClient>> {
    let mut file = match File::open(directory.join("reference.txt")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("Cannot read cached AGY OAuth reference"),
    };
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_REFERENCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("Cannot read cached AGY OAuth reference"))?;
    verify(&bytes, reference).map(Some)
}

async fn load_reference(
    http: &reqwest::Client,
    root: &Path,
    reference: &Reference,
) -> Result<OAuthClient> {
    let cache_root = root.to_owned();
    let pinned = reference.clone();
    let cached = tokio::task::spawn_blocking(move || {
        let directory = cache_directory(&cache_root, &pinned)
            .map_err(|_| anyhow::anyhow!("Cannot initialize private AGY OAuth reference cache"))?;
        let _guard = lock_file(&directory.join("cache.lock"))
            .map_err(|_| anyhow::anyhow!("Cannot lock AGY OAuth reference cache"))?;
        read_cache(&directory, &pinned)
    })
    .await
    .map_err(|_| anyhow::anyhow!("AGY OAuth reference cache task failed"))??;
    if let Some(client) = cached {
        return Ok(client);
    }

    // No incoming auth, account ID or OAuth credential is sent to the reference host.
    // The caller's HTTP client has no default authorization and disables redirects.
    let response = http.get(&reference.url).send().await
        .map_err(|_| anyhow::anyhow!("Cannot download pinned AGY OAuth reference; check network access or provide both client environment overrides"))?;
    if !response.status().is_success() {
        bail!(
            "Pinned AGY OAuth reference download failed (HTTP {})",
            response.status().as_u16()
        );
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_REFERENCE_BYTES as u64)
    {
        bail!("AGY OAuth reference exceeds size limit");
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|_| anyhow::anyhow!("Cannot read pinned AGY OAuth reference download"))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_REFERENCE_BYTES {
            bail!("AGY OAuth reference exceeds size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    let cache_root = root.to_owned();
    let pinned = reference.clone();
    tokio::task::spawn_blocking(move || {
        // Verify before publication. Another process may have published the same pin;
        // re-read under the cache lock, without ever holding it across network I/O.
        let client = verify(&bytes, &pinned)?;
        let directory = cache_directory(&cache_root, &pinned)
            .map_err(|_| anyhow::anyhow!("Cannot initialize private AGY OAuth reference cache"))?;
        let _guard = lock_file(&directory.join("cache.lock"))
            .map_err(|_| anyhow::anyhow!("Cannot lock AGY OAuth reference cache"))?;
        if let Some(existing) = read_cache(&directory, &pinned)? {
            return Ok(existing);
        }
        atomic_bytes(&directory.join("reference.txt"), &bytes)
            .map_err(|_| anyhow::anyhow!("Cannot persist verified AGY OAuth reference"))?;
        Ok(client)
    })
    .await
    .map_err(|_| anyhow::anyhow!("AGY OAuth reference cache task failed"))?
}

fn verify(bytes: &[u8], reference: &Reference) -> Result<OAuthClient> {
    if bytes.len() > MAX_REFERENCE_BYTES {
        bail!("AGY OAuth reference exceeds size limit");
    }
    if Sha256::digest(bytes).as_slice() != reference.digest {
        bail!(
            "AGY OAuth reference integrity check failed; refusing OAuth exchange (cached reference may be corrupt)"
        );
    }
    parse(bytes)
}

fn parse(bytes: &[u8]) -> Result<OAuthClient> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("Invalid AGY OAuth reference data"))?;
    let marker = "export const OAUTH_CONFIG = {";
    if text.matches(marker).count() != 1 {
        bail!("Invalid AGY OAuth reference configuration block");
    }
    let block = text
        .split_once(marker)
        .and_then(|(_, tail)| tail.split_once("\n};"))
        .map(|(block, _)| block)
        .ok_or_else(|| anyhow::anyhow!("Invalid AGY OAuth reference configuration block"))?;
    fn field(block: &str, name: &str) -> Result<String> {
        let prefix = format!("{name}:");
        let mut found = None;
        for line in block.lines() {
            if let Some(value) = line.trim().strip_prefix(&prefix) {
                if found.is_some() {
                    bail!("Duplicate AGY OAuth reference client field");
                }
                let value = value
                    .trim()
                    .strip_suffix(',')
                    .unwrap_or(value.trim())
                    .trim();
                let value = value
                    .strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
                    .filter(|value| {
                        !value.is_empty()
                            && value.len() <= 1024
                            && value
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
                    })
                    .ok_or_else(|| anyhow::anyhow!("Invalid AGY OAuth reference client field"))?;
                found = Some(value.to_owned());
            }
        }
        found.ok_or_else(|| anyhow::anyhow!("Missing AGY OAuth reference client field"))
    }
    OAuthClient::new(field(block, "clientId")?, field(block, "clientSecret")?)
}
