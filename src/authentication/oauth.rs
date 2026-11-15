//! Shared OAuth refresh admission, locking, generation-safe publication and shutdown.
use super::agy::AgyEndpoints;
use crate::accounts::{Credential, Store, now};
use anyhow::{Result, bail};
use futures_util::StreamExt;
use serde::de::DeserializeOwned;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, Semaphore, oneshot},
    task::JoinSet,
};

#[derive(Clone)]
pub(super) enum Flavor {
    Codex,
    Agy(AgyEndpoints),
}
impl Flavor {
    fn provider(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Agy(_) => "agy",
        }
    }
}
#[derive(Clone)]
pub struct AuthClient {
    pub(super) client: reqwest::Client,
    pub(super) flavor: Flavor,
    pub(super) issuer: String,
    refresh_tasks: Arc<Mutex<JoinSet<()>>>,
    refresh_admission: Arc<Semaphore>,
    closing: Arc<AtomicBool>,
}
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error("Account was removed or its login generation changed")]
    AccountChanged,
    #[error("Cannot read or persist account credentials")]
    Storage(#[source] anyhow::Error),
    #[error("Account session was revoked; sign in again")]
    Revoked,
    #[error("Unable to renew account credential")]
    Other(#[source] anyhow::Error),
}
impl From<anyhow::Error> for RefreshError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}
impl AuthClient {
    pub(super) fn ensure_refresh_open(&self) -> Result<()> {
        if self.closing.load(Ordering::Acquire) {
            bail!("OAuth client is shutting down");
        }
        Ok(())
    }

    pub(super) fn with_flavor(issuer: String, flavor: Flavor) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            issuer,
            flavor,
            refresh_tasks: Arc::new(Mutex::new(JoinSet::new())),
            refresh_admission: Arc::new(Semaphore::new(64)),
            closing: Arc::new(AtomicBool::new(false)),
        })
    }
    pub async fn refresh(
        &self,
        store: &Store,
        id: &str,
        rejected_access: Option<&str>,
    ) -> Result<Credential, RefreshError> {
        self.admit_refresh(store, id, None, rejected_access).await
    }
    pub async fn refresh_for_generation(
        &self,
        store: &Store,
        id: &str,
        generation: &str,
        rejected_access: Option<&str>,
    ) -> Result<Credential, RefreshError> {
        self.admit_refresh(store, id, Some(generation), rejected_access)
            .await
    }
    async fn admit_refresh(
        &self,
        store: &Store,
        id: &str,
        generation: Option<&str>,
        rejected_access: Option<&str>,
    ) -> Result<Credential, RefreshError> {
        if self.closing.load(Ordering::Acquire) {
            return Err(anyhow::anyhow!("OAuth client is shutting down").into());
        }
        let mut tasks = self.refresh_tasks.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(anyhow::anyhow!("OAuth client is shutting down").into());
        }
        // Bound both live transactions and retained handles, even with cancelled callers.
        while let Some(result) = tasks.try_join_next() {
            if result.is_err() {
                tracing::warn!(event = "credential_refresh_task_failed");
            }
        }
        let permit = self
            .refresh_admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("OAuth refresh capacity reached"))?;
        let auth = self.clone();
        let store = store.clone();
        let id = id.to_owned();
        let generation = generation.map(str::to_owned);
        let rejected_access = rejected_access.map(str::to_owned);
        let (sender, receiver) = oneshot::channel();
        // This task owns the cross-process lock through parsing and durable publication.
        // Dropping a request only drops the receiver, never a possibly rotating exchange.
        tasks.spawn(async move {
            let _permit = permit;
            let result = auth
                .refresh_transaction(
                    &store,
                    &id,
                    generation.as_deref(),
                    rejected_access.as_deref(),
                )
                .await;
            let _ = sender.send(result);
        });
        drop(tasks);
        receiver
            .await
            .map_err(|_| anyhow::anyhow!("OAuth refresh task failed"))?
    }
    /// Close admission and drain admitted rotations. Process kills cannot be made lossless.
    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        let mut tasks = self.refresh_tasks.lock().await;
        while let Some(result) = tasks.join_next().await {
            if result.is_err() {
                tracing::warn!(event = "credential_refresh_task_failed");
            }
        }
    }
    async fn refresh_transaction(
        &self,
        store: &Store,
        id: &str,
        generation: Option<&str>,
        rejected_access: Option<&str>,
    ) -> Result<Credential, RefreshError> {
        let _guard = loop {
            if self.closing.load(Ordering::Acquire) {
                return Err(anyhow::anyhow!("OAuth client is shutting down").into());
            }
            let guard_store = store.clone();
            let guard_id = id.to_owned();
            let guard =
                tokio::task::spawn_blocking(move || guard_store.try_refresh_lock(&guard_id))
                    .await
                    .map_err(|error| RefreshError::Storage(error.into()))?
                    .map_err(RefreshError::Storage)?;
            if let Some(guard) = guard {
                break guard;
            }
            // Do not strand a blocking lock waiter beyond the shutdown grace period.
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        let read_store = store.clone();
        let read_id = id.to_owned();
        let read_generation = generation.map(str::to_owned);
        let current = tokio::task::spawn_blocking(move || match read_generation {
            Some(generation) => read_store.lease_for_generation(&read_id, &generation),
            None => read_store.lease(&read_id).map(Some),
        })
        .await
        .map_err(|error| RefreshError::Storage(error.into()))?
        .map_err(RefreshError::Storage)?
        .ok_or(RefreshError::AccountChanged)?;
        // Check the leased provider, including the fresh-credential fast path: a
        // miswired client must never send another provider's credential upstream.
        if current.account.provider != self.flavor.provider() {
            return Err(anyhow::anyhow!("OAuth client provider does not match account").into());
        }
        let current = current.credential;
        // Queued lock waiters must not start another exchange while shutdown drains.
        if self.closing.load(Ordering::Acquire) {
            return Err(anyhow::anyhow!("OAuth client is shutting down").into());
        }
        if let Some(rejected) = rejected_access {
            if current.access_token != rejected {
                return Ok(current);
            }
        } else if current.expires_at > now().saturating_add(300) {
            return Ok(current);
        }
        tracing::debug!(account_id = id, event = "credential_refresh_started");
        let refreshed = match &self.flavor {
            Flavor::Codex => self.codex_refresh(&current).await,
            Flavor::Agy(_) => self.agy_refresh(&current).await,
        };
        let refreshed = match refreshed {
            Ok(credential) => credential,
            Err(RefreshError::Revoked) => {
                let s = store.clone();
                let revoked_id = id.to_owned();
                let generation = current.generation.clone();
                let still_current = tokio::task::spawn_blocking(move || {
                    s.mark_needs_login(&revoked_id, &generation)?;
                    s.lease_for_generation(&revoked_id, &generation)
                        .map(|lease| lease.is_some())
                })
                .await
                .map_err(|error| RefreshError::Storage(error.into()))?
                .map_err(RefreshError::Storage)?;
                if !still_current {
                    return Err(RefreshError::AccountChanged);
                }
                tracing::warn!(account_id = id, event = "credential_revoked");
                return Err(RefreshError::Revoked);
            }
            Err(error) => {
                tracing::warn!(account_id = id, event = "credential_refresh_failed");
                return Err(error);
            }
        };
        let s = store.clone();
        let refresh_id = id.to_owned();
        let result = tokio::task::spawn_blocking(move || {
            s.finish_refresh_for_generation(
                &refresh_id,
                &current.generation,
                &current.access_token,
                refreshed,
            )
        })
        .await
        .map_err(|error| RefreshError::Storage(error.into()))?
        .map_err(RefreshError::Storage)?
        .ok_or(RefreshError::AccountChanged);
        match &result {
            Ok(_) => tracing::info!(account_id = id, event = "credential_refresh_completed"),
            Err(_) => tracing::warn!(
                account_id = id,
                phase = "storage",
                event = "credential_refresh_failed"
            ),
        }
        result
    }
}

pub(super) async fn read_json<T: DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| anyhow::anyhow!("OAuth response read failed"))?;
        if bytes.len().saturating_add(chunk.len()) > 512 * 1024 {
            bail!("OAuth response exceeds size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("Invalid OAuth JSON response"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(store: &Store) -> crate::accounts::Account {
        store
            .login(
                "synthetic",
                Credential {
                    access_token: "synthetic-access".into(),
                    refresh_token: "synthetic-refresh".into(),
                    id_token: None,
                    expires_at: u64::MAX,
                    subject: "synthetic-user".into(),
                    account_id: "synthetic-workspace".into(),
                    generation: String::new(),
                },
            )
            .unwrap()
    }

    #[tokio::test]
    async fn refresh_tracker_prunes_completed_tasks_and_bounds_admission() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let selected = account(&store);
        let auth = AuthClient::with_issuer("http://127.0.0.1:1".into()).unwrap();
        for _ in 0..128 {
            auth.refresh_for_generation(&store, &selected.id, &selected.generation, None)
                .await
                .unwrap();
            assert!(auth.refresh_tasks.lock().await.len() <= 2);
        }
        let permits = auth
            .refresh_admission
            .clone()
            .acquire_many_owned(64)
            .await
            .unwrap();
        assert!(auth.refresh(&store, &selected.id, None).await.is_err());
        drop(permits);
        auth.shutdown().await;
        assert!(auth.refresh_tasks.lock().await.is_empty());
        assert!(auth.refresh(&store, &selected.id, None).await.is_err());
    }

    #[tokio::test]
    async fn shutdown_stops_lock_waiters_without_waiting_for_another_process() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let selected = account(&store);
        let guard = store.refresh_lock(&selected.id).unwrap();
        let auth = AuthClient::with_issuer("http://127.0.0.1:1".into()).unwrap();
        let caller = {
            let auth = auth.clone();
            let store = store.clone();
            let id = selected.id.clone();
            tokio::spawn(async move { auth.refresh(&store, &id, None).await })
        };
        while auth.refresh_admission.available_permits() == 64 {
            tokio::task::yield_now().await;
        }
        tokio::time::timeout(Duration::from_secs(2), auth.shutdown())
            .await
            .unwrap();
        assert!(caller.await.unwrap().is_err());
        drop(guard);
    }
}
