//! Account command workflows, independent of CLI parsing and output formatting.
use crate::{
    domain::account::{AccountSnapshot, Credential},
    ports::account_repository::AccountCommandRepository,
};
use anyhow::Result;
use std::{future::Future, sync::Arc};

pub async fn login<R, F>(store: R, name: String, authenticate: F) -> Result<AccountSnapshot>
where
    R: AccountCommandRepository + 'static,
    F: Future<Output = Result<Credential>>,
{
    login_for(store, "codex".into(), name, authenticate).await
}

pub async fn login_for<R, F>(
    store: R,
    provider: String,
    name: String,
    authenticate: F,
) -> Result<AccountSnapshot>
where
    R: AccountCommandRepository + 'static,
    F: Future<Output = Result<Credential>>,
{
    anyhow::ensure!(
        !name.trim().is_empty() && name.len() <= 200,
        "Account name must contain 1–200 bytes"
    );
    let store = Arc::new(store);
    let login_store = store.clone();
    let ticket = tokio::task::spawn_blocking(move || login_store.begin_login(&provider)).await??;
    // Persist the ticket before authentication so stale completions cannot revive accounts.
    let credential = authenticate.await?;
    tokio::task::spawn_blocking(move || store.finish_login(ticket, &name, credential)).await?
}

pub async fn remove<R: AccountCommandRepository + 'static>(store: R, id: String) -> Result<()> {
    tokio::task::spawn_blocking(move || store.remove(&id)).await?
}
