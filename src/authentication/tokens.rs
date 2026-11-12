//! Decode trusted OAuth responses into proxy-owned account identities.
use crate::accounts::{Credential, now};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::Value;
#[derive(Deserialize)]
pub(super) struct Tokens {
    pub(super) access_token: String,
    pub(super) refresh_token: Option<String>,
    pub(super) id_token: Option<String>,
    pub(super) expires_in: Option<u64>,
}
fn claims(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    parts.next()?;
    let payload = parts.next()?;
    if parts.next().is_none() || parts.next().is_some() || payload.len() > 128 * 1024 {
        return None;
    }
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?).ok()
}
pub(super) fn credential_from_tokens(
    tokens: Tokens,
    previous: Option<&Credential>,
) -> Result<Credential> {
    if tokens.access_token.is_empty() {
        bail!("OAuth returned an empty access token");
    }
    // Claims only identify a token delivered over the OAuth TLS channel; they are
    // never accepted from incoming proxy requests as proof of authorization.
    let id_claims = tokens.id_token.as_deref().and_then(claims);
    let access_claims = claims(&tokens.access_token);
    let subject = id_claims
        .as_ref()
        .and_then(|c| c.get("sub"))
        .and_then(Value::as_str)
        .or_else(|| {
            access_claims
                .as_ref()
                .and_then(|c| c.pointer("/https:~1~1api.openai.com~1auth/chatgpt_user_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| previous.map(|c| c.subject.as_str()))
        .context("OAuth did not identify the user; account was not added")?
        .to_owned();
    let workspace = [id_claims.as_ref(), access_claims.as_ref()]
        .into_iter()
        .flatten()
        .find_map(|c| {
            c.get("chatgpt_account_id")
                .or_else(|| c.pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| previous.map(|c| c.account_id.as_str()))
        .context("OAuth did not identify the ChatGPT workspace")?
        .to_owned();
    let expires_at = tokens
        .expires_in
        .map(|seconds| now().saturating_add(seconds))
        .or_else(|| access_claims.as_ref()?.get("exp")?.as_u64())
        .unwrap_or_else(|| now().saturating_add(3600));
    Ok(Credential {
        access_token: tokens.access_token,
        refresh_token: tokens
            .refresh_token
            .or_else(|| previous.map(|c| c.refresh_token.clone()))
            .filter(|s| !s.is_empty())
            .context("OAuth did not return a refresh token")?,
        id_token: tokens
            .id_token
            .or_else(|| previous.and_then(|c| c.id_token.clone())),
        expires_at,
        subject,
        account_id: workspace,
        generation: previous.map(|c| c.generation.clone()).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn jwt(claims: Value) -> String {
        format!(
            "e30.{}.signature",
            URL_SAFE_NO_PAD.encode(claims.to_string())
        )
    }
    #[test]
    fn oauth_identity_distinguishes_users_in_same_workspace_and_retains_refresh() {
        let token = |user: &str| Tokens {
            access_token: jwt(json!({"exp":9999999999u64})),
            refresh_token: Some("refresh".into()),
            id_token: Some(jwt(
                json!({"sub":user,"https://api.openai.com/auth":{"chatgpt_account_id":"team"}}),
            )),
            expires_in: Some(3600),
        };
        let a = credential_from_tokens(token("a"), None).unwrap();
        let b = credential_from_tokens(token("b"), None).unwrap();
        assert_ne!(a.subject, b.subject);
        assert_eq!(a.account_id, b.account_id);
        let refreshed = credential_from_tokens(
            Tokens {
                access_token: "new".into(),
                refresh_token: None,
                id_token: None,
                expires_in: Some(1000),
            },
            Some(&a),
        )
        .unwrap();
        assert_eq!(refreshed.refresh_token, "refresh");
        assert_eq!(refreshed.subject, "a");
    }
    #[test]
    fn incomplete_fresh_identity_and_empty_tokens_are_rejected() {
        assert!(
            credential_from_tokens(
                Tokens {
                    access_token: "access".into(),
                    refresh_token: Some("r".into()),
                    id_token: None,
                    expires_in: None
                },
                None
            )
            .is_err()
        );
        assert!(claims("not-a-jwt").is_none());
        assert!(claims("a.!!!!.b").is_none());
    }
}
