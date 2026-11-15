//! Account telemetry and confirmed manual reset workflows; independent of CLI/HTTP.
use crate::{
    accounts::{Account, Store, now},
    domain::account_usage::*,
    ports::account_usage::AccountUsageProvider,
};
use futures_util::{StreamExt, stream};
use std::{fs::File, sync::Arc};
use uuid::Uuid;

pub struct AccountUsageService {
    store: Store,
    providers: Vec<Arc<dyn AccountUsageProvider>>,
}
impl AccountUsageService {
    pub fn new(store: Store, providers: Vec<Arc<dyn AccountUsageProvider>>) -> Self {
        Self { store, providers }
    }

    fn provider(&self, name: &str) -> Option<Arc<dyn AccountUsageProvider>> {
        self.providers.iter().find(|p| p.name() == name).cloned()
    }

    async fn accounts(&self) -> Result<crate::accounts::Registry, UsageError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.list())
            .await
            .map_err(|_| UsageError::Storage)?
            .map_err(|_| UsageError::Storage)
    }

    async fn account(&self, provider: &str, id: &str) -> Result<Account, UsageError> {
        if Uuid::parse_str(id).is_err() {
            return Err(UsageError::NotFound);
        }
        let account = self
            .accounts()
            .await?
            .accounts
            .into_iter()
            .find(|account| account.id == id)
            .ok_or(UsageError::NotFound)?;
        if account.provider != provider {
            return Err(UsageError::ProviderMismatch);
        }
        Ok(account)
    }

    pub async fn shutdown(&self) {
        for provider in &self.providers {
            provider.shutdown().await;
        }
    }

    pub async fn status(
        &self,
        provider: Option<&str>,
        refresh: bool,
    ) -> Result<StatusReport, UsageError> {
        let registry = self.accounts().await?;
        if let Some(name) = provider
            && self.provider(name).is_none()
            && !registry
                .accounts
                .iter()
                .any(|account| account.provider == name)
        {
            return Err(UsageError::UnknownProvider);
        }
        let accounts: Vec<_> = registry
            .accounts
            .into_iter()
            .filter(|account| provider.is_none_or(|name| account.provider == name))
            .collect();
        if accounts.len() > 4096 {
            return Err(UsageError::Capacity);
        }
        let active = &registry.active;
        let mut results = stream::iter(accounts.into_iter().enumerate().map(|(index, account)| {
            let implementation = self.provider(&account.provider);
            async move {
                let (usage, error) = match (&implementation, &account.usage) {
                    (Some(p), Some(snapshot)) => match p.decode_cached(snapshot) {
                        Ok(data) => (data, None),
                        Err(error) => (UsageData::default(), Some(error)),
                    },
                    (Some(_), None) => (UsageData::default(), None),
                    (None, _) => (UsageData::unsupported(), None),
                };
                let mut view = AccountUsageView {
                    id: account.id.clone(),
                    name: account.name.clone(),
                    provider: account.provider.clone(),
                    active: active.get(&account.provider) == Some(&account.id),
                    status: account.status.clone(),
                    capabilities: implementation
                        .as_ref()
                        .map(|p| p.capabilities())
                        .unwrap_or_default(),
                    usage,
                    observation: account.usage_observation.clone().unwrap_or_default(),
                    refresh_error: error,
                    warning: None,
                    pending_reset_id: None,
                };
                let store = self.store.clone();
                let id = account.id.clone();
                match tokio::task::spawn_blocking(move || store.reset_attempt(&id)).await {
                    Ok(Ok(Some(attempt)))
                        if matches!(
                            attempt.state,
                            ResetAttemptState::Pending | ResetAttemptState::Unknown
                        ) =>
                    {
                        view.pending_reset_id = Some(attempt.request_id);
                    }
                    Ok(Ok(_)) => {}
                    _ => view.warning = Some(UsageError::Storage),
                }
                if refresh {
                    match implementation {
                        Some(p) if p.capabilities().live_read => {
                            let started = now();
                            match p.read(account.clone()).await {
                                Ok(read) => {
                                    let observation = UsageObservation {
                                        observed_at: Some(started),
                                        source: UsageSource::Live,
                                    };
                                    match self
                                        .save_usage(&account, &read, observation.clone())
                                        .await
                                    {
                                        Ok(()) => {
                                            view.refresh_error = None;
                                            if read.data.ordinary_usage_allowed == Some(true) {
                                                let store = self.store.clone();
                                                let observed = account.clone();
                                                match tokio::task::spawn_blocking(move || {
                                                    store.reconcile_refreshed_quota(&observed)
                                                })
                                                .await
                                                {
                                                    Ok(Ok(Some(status))) => view.status = status,
                                                    Ok(Ok(None)) => {
                                                        view.refresh_error =
                                                            Some(UsageError::AccountChanged);
                                                    }
                                                    _ => {
                                                        view.refresh_error =
                                                            Some(UsageError::Storage)
                                                    }
                                                }
                                            }
                                            view.usage = read.data;
                                            view.observation = observation;
                                            view.warning = read.warning.or(view.warning);
                                        }
                                        Err(error) => view.refresh_error = Some(error),
                                    }
                                }
                                Err(error) => view.refresh_error = Some(error),
                            }
                        }
                        _ => view.refresh_error = Some(UsageError::Unsupported),
                    }
                }
                (index, view)
            }
        }))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
        results.sort_by_key(|(index, _)| *index);
        let accounts: Vec<_> = results.into_iter().map(|(_, account)| account).collect();
        let has_errors = accounts
            .iter()
            .any(|account| account.refresh_error.is_some() || account.warning.is_some());
        Ok(StatusReport {
            accounts,
            has_errors,
        })
    }

    async fn save_usage(
        &self,
        account: &Account,
        read: &UsageRead,
        observation: UsageObservation,
    ) -> Result<(), UsageError> {
        let store = self.store.clone();
        let id = account.id.clone();
        let generation = account.generation.clone();
        let snapshot = read.snapshot.clone();
        let current = tokio::task::spawn_blocking(move || {
            store.record_usage_observed(&id, &generation, snapshot, observation)
        })
        .await
        .map_err(|_| UsageError::Storage)?
        .map_err(|_| UsageError::Storage)?;
        if !current {
            return Err(UsageError::AccountChanged);
        }
        Ok(())
    }

    async fn reset_lock(&self, id: &str) -> Result<Arc<File>, UsageError> {
        let store = self.store.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || store.try_quota_reset_lock(&id))
            .await
            .map_err(|_| UsageError::Storage)?
            .map_err(|_| UsageError::Storage)?
            .map(Arc::new)
            .ok_or(UsageError::Busy)
    }

    async fn journal(&self, id: &str) -> Result<Option<ResetAttempt>, UsageError> {
        let store = self.store.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || store.reset_attempt(&id))
            .await
            .map_err(|_| UsageError::Storage)?
            .map_err(|_| UsageError::Storage)
    }

    async fn save_attempt(
        &self,
        attempt: &ResetAttempt,
        guard: Arc<File>,
    ) -> Result<(), UsageError> {
        let store = self.store.clone();
        let attempt = attempt.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            store.save_reset_attempt(&attempt)
        })
        .await
        .map_err(|_| UsageError::Storage)?
        .map_err(|_| UsageError::Storage)
    }

    async fn read_and_save(
        &self,
        implementation: &dyn AccountUsageProvider,
        account: &Account,
    ) -> Result<UsageRead, UsageError> {
        let observed_at = now();
        let read = implementation.read(account.clone()).await?;
        self.save_usage(
            account,
            &read,
            UsageObservation {
                observed_at: Some(observed_at),
                source: UsageSource::Live,
            },
        )
        .await?;
        Ok(read)
    }

    pub async fn reset(
        &self,
        provider: &str,
        id: &str,
        confirm: bool,
        retry: Option<&str>,
    ) -> Result<ResetReport, UsageError> {
        let mut account = self.account(provider, id).await?;
        let implementation = self.provider(provider).ok_or(UsageError::Unsupported)?;
        if !implementation.capabilities().reset {
            return Err(UsageError::Unsupported);
        }
        let guard = self.reset_lock(id).await?;
        let previous = self.journal(id).await?;
        let mut report = ResetReport {
            provider: provider.into(),
            account_id: id.into(),
            request_id: previous.as_ref().map(|attempt| attempt.request_id.clone()),
            preview: !confirm,
            available: Metric::Unknown,
            outcome: None,
            outcome_unknown: previous.as_ref().is_some_and(|attempt| {
                matches!(
                    attempt.state,
                    ResetAttemptState::Pending | ResetAttemptState::Unknown
                )
            }),
            error: None,
            refresh_error: None,
            reconciled: false,
        };
        if !confirm {
            match self.read_and_save(implementation.as_ref(), &account).await {
                Ok(read) => {
                    report.available = reset_available(&read.data);
                    report.refresh_error = read.warning;
                }
                Err(error) => report.refresh_error = Some(error),
            }
            return Ok(report);
        }
        let mut attempt = if let Some(request_id) = retry {
            let previous = previous.ok_or(UsageError::InvalidRetry)?;
            if previous.request_id != request_id
                || previous.provider != provider
                || previous.generation != account.generation
            {
                return Err(UsageError::InvalidRetry);
            }
            previous
        } else {
            if previous.as_ref().is_some_and(|attempt| {
                matches!(
                    attempt.state,
                    ResetAttemptState::Pending | ResetAttemptState::Unknown
                )
            }) {
                report.error = Some(UsageError::PendingReset);
                report.outcome_unknown = true;
                return Ok(report);
            }
            let read = self
                .read_and_save(implementation.as_ref(), &account)
                .await?;
            report.available = reset_available(&read.data);
            match report.available {
                Metric::Known(0) => return Err(UsageError::NoResetCredits),
                Metric::Known(_) => {}
                _ => return Err(UsageError::UnknownResetCredits),
            }
            let current = self.account(provider, id).await?;
            if current.generation != account.generation {
                return Err(UsageError::AccountChanged);
            }
            account = current;
            ResetAttempt {
                provider: provider.into(),
                account_id: id.into(),
                generation: account.generation.clone(),
                request_id: Uuid::new_v4().to_string(),
                status_revision: account.status_revision,
                created_at: now(),
                state: ResetAttemptState::Pending,
            }
        };
        report.request_id = Some(attempt.request_id.clone());
        let previously_uncertain = retry.is_some()
            && matches!(
                attempt.state,
                ResetAttemptState::Pending | ResetAttemptState::Unknown
            );
        if let ResetAttemptState::Confirmed(outcome) = &attempt.state {
            report.outcome = Some(outcome.clone());
            report.outcome_unknown = false;
        } else {
            attempt.state = ResetAttemptState::Pending;
            self.save_attempt(&attempt, guard.clone()).await?;
            match implementation
                .reset(account.clone(), attempt.request_id.clone())
                .await
            {
                Ok(outcome) => {
                    attempt.state = ResetAttemptState::Confirmed(outcome.clone());
                    report.outcome = Some(outcome);
                    report.outcome_unknown = false;
                }
                Err(failure) => {
                    let uncertain = failure.dispatched || previously_uncertain;
                    attempt.state = if uncertain {
                        ResetAttemptState::Unknown
                    } else {
                        ResetAttemptState::NotDispatched
                    };
                    report.outcome_unknown = uncertain;
                    report.error = Some(failure.error);
                }
            }
            if self.save_attempt(&attempt, guard.clone()).await.is_err() {
                // The pre-dispatch journal is durable even if saving the response failed.
                report.error = Some(UsageError::Storage);
                report.outcome_unknown = matches!(
                    attempt.state,
                    ResetAttemptState::Pending | ResetAttemptState::Unknown
                );
                return Ok(report);
            }
        }
        if report.outcome.is_some() {
            match self.read_and_save(implementation.as_ref(), &account).await {
                Ok(read) => {
                    report.available = reset_available(&read.data);
                    report.refresh_error = read.warning;
                    if read.data.ordinary_usage_allowed == Some(true) {
                        let store = self.store.clone();
                        let attempt = attempt.clone();
                        let save_guard = guard.clone();
                        match tokio::task::spawn_blocking(move || {
                            let _guard = save_guard;
                            store.reconcile_quota_reset(&attempt, true)
                        })
                        .await
                        {
                            Ok(Ok(reconciled)) => report.reconciled = reconciled,
                            _ => report.error = Some(UsageError::Storage),
                        }
                    }
                }
                Err(error) => report.refresh_error = Some(error),
            }
        }
        Ok(report)
    }
}

fn reset_available(data: &UsageData) -> Metric<u64> {
    match &data.reset_credits {
        Metric::Known(credits) => credits.available.clone(),
        Metric::Unknown => Metric::Unknown,
        Metric::Unsupported => Metric::Unsupported,
    }
}
