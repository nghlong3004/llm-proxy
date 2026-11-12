//! Account policy and provider Strategy dispatch, shared by all protocol adapters.
use super::AppState;
use crate::{
    accounts::{Account, now},
    domain::error::{FailureKind, ProxyError},
    domain::*,
};
use futures_util::StreamExt;
use std::{collections::HashSet, sync::Arc};

pub fn generate(
    state: AppState,
    request: Arc<GenerationRequest>,
    context: RequestContext,
) -> EventStream {
    Box::pin(async_stream::stream! {
        let mut excluded = HashSet::new();
        let mut committed = false;
        let mut shutdown = state.shutdown.subscribe();

        for _ in 0..64 {
            if *shutdown.borrow() {
                yield Err(shutdown_error());
                return;
            }
            let store = state.store.clone();
            let skip = excluded.clone();
            let provider_name = state.config.default_provider.clone();
            let lease = match tokio::task::spawn_blocking(move || {
                store.select_for_provider_at(&provider_name, &skip, now())
            }).await {
                Ok(Ok(lease)) => lease,
                Ok(Err(error)) => { yield Err(error.into()); return; }
                Err(_) => { yield Err(ProxyError::storage()); return; }
            };
            let account = lease.account;
            excluded.insert(account.id.clone());
            let provider = match state.providers.get(&state.config.default_provider) {
                Ok(provider) => provider,
                Err(error) => { yield Err(error); return; }
            };
            tracing::debug!(
                request_id = %context.request_id,
                account_id = %account.id,
                provider = provider.name(),
                requested_model = %request.model,
                event = "provider_attempt"
            );

            let attempt = tokio::select! {
                result = provider.generate(request.clone(), account.clone(), context.clone()) => result,
                _ = shutdown.changed() => { yield Err(shutdown_error()); return; }
            };
            let mut stream = match attempt {
                Ok(stream) => stream,
                Err(error) => {
                    if error.kind == FailureKind::AccountChanged {
                        excluded.remove(&account.id);
                    }
                    match handle_failure(&state, &account, &error, committed, &context.request_id).await {
                        Ok(true) => continue,
                        Ok(false) => yield Err(error),
                        Err(storage) => yield Err(storage),
                    }
                    return;
                }
            };
            let mut retry = false;
            loop {
                let event = tokio::select! {
                    event = stream.next() => event,
                    _ = shutdown.changed() => { yield Err(shutdown_error()); return; }
                };
                match event {
                    Some(Ok(GenerationEvent::QuotaSnapshot(usage))) => {
                        let store = state.store.clone();
                        let id = account.id.clone();
                        let generation = account.generation.clone();
                        let saved = tokio::task::spawn_blocking(move || {
                            store.record_usage(&id, &generation, usage)
                        }).await;
                        if !matches!(saved, Ok(Ok(()))) {
                            yield Err(ProxyError::storage());
                            return;
                        }
                    }
                    Some(Ok(event)) => {
                        let finished = matches!(event, GenerationEvent::Finish { .. });
                        if !matches!(event, GenerationEvent::Ping) {
                            committed = true;
                        }
                        yield Ok(event);
                        if finished { return; }
                    }
                    Some(Err(error)) => {
                        match handle_failure(&state, &account, &error, committed, &context.request_id).await {
                            Ok(true) => retry = true,
                            Ok(false) => yield Err(error),
                            Err(storage) => yield Err(storage),
                        }
                        break;
                    }
                    None => {
                        yield Err(ProxyError::protocol("Provider ended before reporting completion"));
                        return;
                    }
                }
            }
            if !retry { return; }
        }
        yield Err(ProxyError::new(
            429, FailureKind::RateLimit, "Account failover attempt limit reached",
        ));
    })
}

async fn handle_failure(
    state: &AppState,
    account: &Account,
    error: &ProxyError,
    committed: bool,
    request_id: &str,
) -> Result<bool, ProxyError> {
    let store = state.store.clone();
    let id = account.id.clone();
    let generation = account.generation.clone();
    let retry = if let Some(reset) = error.quota_reset() {
        tokio::task::spawn_blocking(move || store.mark_quota_at(&id, &generation, reset, now()))
            .await
            .map_err(|_| ProxyError::storage())?
            .map_err(|_| ProxyError::storage())?;
        true
    } else if error.kind == FailureKind::Authentication {
        tokio::task::spawn_blocking(move || store.mark_needs_login(&id, &generation))
            .await
            .map_err(|_| ProxyError::storage())?
            .map_err(|_| ProxyError::storage())?;
        true
    } else {
        error.kind == FailureKind::AccountChanged
    };
    tracing::warn!(
        request_id, account_id = %account.id, kind = ?error.kind, error_code = error.code, committed,
        upstream_status = error.upstream_status, upstream_parameter = error.upstream_parameter,
        retry = retry && !committed, event = "upstream_failure"
    );
    Ok(retry && !committed)
}
fn shutdown_error() -> ProxyError {
    ProxyError::new(503, FailureKind::Upstream, "Proxy is shutting down")
}
