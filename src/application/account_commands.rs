//! Account command workflows, independent of CLI parsing and output formatting.
use crate::accounts::{Account, Credential, Registry, Store};
use anyhow::Result;
use std::future::Future;

pub async fn login<F>(store: Store, name: String, authenticate: F) -> Result<Account>
where
    F: Future<Output = Result<Credential>>,
{
    login_for(store, "codex".into(), name, authenticate).await
}

pub async fn login_for<F>(
    store: Store,
    provider: String,
    name: String,
    authenticate: F,
) -> Result<Account>
where
    F: Future<Output = Result<Credential>>,
{
    anyhow::ensure!(
        !name.trim().is_empty() && name.len() <= 200,
        "Account name must contain 1–200 bytes"
    );
    let login_store = store.clone();
    let ticket = tokio::task::spawn_blocking(move || login_store.begin_login(&provider)).await??;
    // Persist the ticket before authentication so stale completions cannot revive accounts.
    let credential = authenticate.await?;
    tokio::task::spawn_blocking(move || store.finish_login(ticket, &name, credential)).await?
}

pub async fn list(store: Store) -> Result<Registry> {
    tokio::task::spawn_blocking(move || store.list()).await?
}

pub async fn remove(store: Store, id: String) -> Result<()> {
    tokio::task::spawn_blocking(move || store.remove(&id)).await?
}
