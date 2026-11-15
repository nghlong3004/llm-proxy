//! HMAC envelopes separate AGY signatures from all other providers and login generations.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

const PREFIX: &str = "llmp:agy:v1:";
const MAX_SIGNATURE: usize = 12 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub struct Replay {
    account: String,
    generation: String,
    model: String,
    pub summary: String,
    pub thought_signature: String,
}

#[derive(Clone)]
pub struct ReasoningSigner {
    key: [u8; 32],
}

impl ReasoningSigner {
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub fn sign(
        &self,
        account: &str,
        generation: &str,
        model: &str,
        summary: &str,
        signature: &str,
    ) -> Option<String> {
        if signature.is_empty() || signature.len().saturating_add(summary.len()) > 8 * 1024 * 1024 {
            return None;
        }
        let payload = super::encode_bounded(
            &Replay {
                account: account.into(),
                generation: generation.into(),
                model: model.into(),
                summary: summary.into(),
                thought_signature: signature.into(),
            },
            8 * 1024 * 1024,
        )?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).ok()?;
        mac.update(&payload);
        let envelope = format!(
            "{PREFIX}{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        );
        (envelope.len() <= MAX_SIGNATURE).then_some(envelope)
    }

    pub fn verify(
        &self,
        signature: &str,
        account: &str,
        generation: &str,
        model: &str,
    ) -> Option<Replay> {
        if signature.len() > MAX_SIGNATURE {
            return None;
        }
        let (payload, tag) = signature.strip_prefix(PREFIX)?.split_once('.')?;
        let payload = URL_SAFE_NO_PAD.decode(payload).ok()?;
        let tag = URL_SAFE_NO_PAD.decode(tag).ok()?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).ok()?;
        mac.update(&payload);
        mac.verify_slice(&tag).ok()?;
        let replay: Replay = serde_json::from_slice(&payload).ok()?;
        (replay.account == account && replay.generation == generation && replay.model == model)
            .then_some(replay)
    }

    /// Session identity is account/workspace/conversation scoped and never prompt-derived.
    pub fn session_id(&self, account: &str, workspace: &str, conversation: &str) -> Option<String> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).ok()?;
        mac.update(b"agy-session-v1");
        for component in [account, workspace, conversation] {
            mac.update(&(component.len() as u64).to_be_bytes());
            mac.update(component.as_bytes());
        }
        Some(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
    }
}
