//! Signed, account/generation/model-bound envelopes for encrypted Codex reasoning.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
const PREFIX: &str = "llmp:codex:v1:";
const MAX_SIGNATURE: usize = 12 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
pub struct Replay {
    account: String,
    generation: String,
    model: String,
    pub id: String,
    pub encrypted_content: String,
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
        id: &str,
        encrypted: &str,
    ) -> Option<String> {
        if id.is_empty()
            || id.len() > 4096
            || encrypted.is_empty()
            || encrypted.len() > 8 * 1024 * 1024
        {
            return None;
        }
        let payload = serde_json::to_vec(&Replay {
            account: account.into(),
            generation: generation.into(),
            model: model.into(),
            id: id.into(),
            encrypted_content: encrypted.into(),
        })
        .ok()?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).ok()?;
        mac.update(&payload);
        Some(format!(
            "{PREFIX}{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        ))
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
}
