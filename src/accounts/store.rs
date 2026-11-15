//! Private, atomic account storage shared by CLI and server processes.
use super::{Account, AccountStatus, Credential, LoginTicket, SelectionError, now};
use crate::domain::account_usage::{
    ResetAttempt, ResetAttemptState, UsageObservation, UsageSource,
};
use crate::{
    domain::account::AccountCatalog,
    ports::account_repository::{
        AccountCommandRepository, AccountUsageRepository, InferenceAccountRepository, ResetGuard,
    },
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Registry {
    pub version: u32,
    pub active: BTreeMap<String, String>,
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub operation_sequence: u64,
    #[serde(default)]
    pub identity_operations: BTreeMap<String, u64>,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            version: 1,
            active: BTreeMap::new(),
            accounts: vec![],
            operation_sequence: 0,
            identity_operations: BTreeMap::new(),
        }
    }
}
pub struct Lease {
    pub account: Account,
    pub credential: Credential,
}
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
}
impl Store {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        private_dir(&root)?;
        private_dir(&root.join("credentials"))?;
        private_dir(&root.join("locks"))?;
        Ok(Self { root })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn registry_lock(&self) -> Result<File> {
        lock_file(&self.root.join("registry.lock"))
    }
    pub fn refresh_lock(&self, id: &str) -> Result<File> {
        validate_id(id)?;
        lock_file(&self.root.join("locks").join(format!("{id}.lock")))
    }
    pub fn try_refresh_lock(&self, id: &str) -> Result<Option<File>> {
        validate_id(id)?;
        let file = private_open(&self.root.join("locks").join(format!("{id}.lock")))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }
    fn credential_path(&self, id: &str, generation: &str) -> Result<PathBuf> {
        validate_id(id)?;
        validate_id(generation)?;
        Ok(self
            .root
            .join("credentials")
            .join(format!("{id}-{generation}.json")))
    }
    fn read_registry(&self) -> Result<Registry> {
        let registry: Registry = read_json(&self.root.join("accounts.json"))?.unwrap_or_default();
        if registry.version != 1 {
            bail!("Unsupported account registry version");
        }
        for account in &registry.accounts {
            validate_id(&account.id)?;
            validate_id(&account.generation)?;
        }
        Ok(registry)
    }
    fn save_registry(&self, registry: &Registry) -> Result<()> {
        atomic_json(&self.root.join("accounts.json"), registry)
    }
    pub fn list(&self) -> Result<Registry> {
        let _guard = self.registry_lock()?;
        self.read_registry()
    }
    pub fn login(&self, name: &str, credential: Credential) -> Result<Account> {
        self.login_for("codex", name, credential)
    }
    pub fn login_for(&self, provider: &str, name: &str, credential: Credential) -> Result<Account> {
        self.finish_login(self.begin_login(provider)?, name, credential)
    }
    pub fn begin_login(&self, provider: &str) -> Result<LoginTicket> {
        if provider.is_empty()
            || provider.len() > 64
            || !provider
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("Invalid provider name");
        }
        let _guard = self.registry_lock()?;
        let mut registry = self.read_registry()?;
        let sequence = next_operation(&mut registry)?;
        self.save_registry(&registry)?;
        Ok(LoginTicket {
            provider: provider.into(),
            sequence,
        })
    }
    pub fn finish_login(
        &self,
        ticket: LoginTicket,
        name: &str,
        mut credential: Credential,
    ) -> Result<Account> {
        let provider = ticket.provider.as_str();
        if name.trim().is_empty() || name.len() > 200 {
            bail!("Account name must contain 1–200 bytes");
        }
        if credential.subject.is_empty()
            || credential.account_id.is_empty()
            || credential.access_token.is_empty()
            || credential.refresh_token.is_empty()
        {
            bail!("Incomplete account credential");
        }
        let _guard = self.registry_lock()?;
        let mut registry = self.read_registry()?;
        let identity = identity_key(provider, &credential.subject, &credential.account_id)?;
        if registry
            .identity_operations
            .get(&identity)
            .is_some_and(|sequence| *sequence >= ticket.sequence)
        {
            bail!("A newer login or account removal superseded this login; start login again");
        }
        let existing = registry.accounts.iter().position(|a| {
            a.provider == provider
                && a.subject == credential.subject
                && a.workspace == credential.account_id
        });
        let id = existing
            .map(|i| registry.accounts[i].id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        credential.generation = Uuid::new_v4().to_string();
        let account = Account {
            provider: provider.into(),
            id: id.clone(),
            name: name.to_owned(),
            subject: credential.subject.clone(),
            workspace: credential.account_id.clone(),
            generation: credential.generation.clone(),
            status: AccountStatus::Available,
            usage: None,
            usage_observation: None,
            status_revision: 0,
        };
        // Credentials are durable before publishing a new account in the registry.
        atomic_json(
            &self.credential_path(&id, &credential.generation)?,
            &credential,
        )?;
        match existing {
            Some(i) => registry.accounts[i] = account.clone(),
            None => registry.accounts.push(account.clone()),
        }
        registry.active.entry(provider.into()).or_insert(id);
        registry
            .identity_operations
            .insert(identity, ticket.sequence);
        self.save_registry(&registry)?;
        // Publication succeeded: cleanup must not report a misleading login failure.
        // A later login for this identity retries any obsolete files left by I/O failure.
        if self
            .cleanup_credentials(&account.id, Some(&account.generation))
            .is_err()
        {
            tracing::warn!(account_id = %account.id, event = "obsolete_credentials_cleanup_failed");
        }
        Ok(account)
    }
    fn cleanup_credentials(&self, id: &str, keep_generation: Option<&str>) -> Result<()> {
        let directory = self.root.join("credentials");
        let prefix = format!("{id}-");
        let keep = keep_generation.map(|generation| format!("{id}-{generation}.json"));
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&prefix) && keep.as_deref() != Some(name.as_ref()) {
                fs::remove_file(entry.path())?;
            }
        }
        File::open(directory)?.sync_all()?;
        Ok(())
    }
    /// Resolve a unique UUID prefix while holding the same lock used for removal.
    pub fn remove(&self, id: &str) -> Result<()> {
        if id.len() < 8
            || id.len() > 36
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            bail!("Use a full account UUID or an ID prefix of at least 8 characters");
        }
        let prefix = id.to_ascii_lowercase();
        let _guard = self.registry_lock()?;
        let mut registry = self.read_registry()?;
        let mut matches = registry
            .accounts
            .iter()
            .filter(|account| account.id.starts_with(&prefix));
        let account = matches
            .next()
            .context("No account matches that ID or prefix")?
            .clone();
        if matches.next().is_some() {
            bail!("Account ID prefix is ambiguous; use a longer prefix or the full UUID");
        }
        let id = account.id.as_str();
        let sequence = next_operation(&mut registry)?;
        registry.identity_operations.insert(
            identity_key(&account.provider, &account.subject, &account.workspace)?,
            sequence,
        );
        registry.accounts.retain(|a| a.id != id);
        registry.active.retain(|_, active| active != id);
        // Unpublish first; a late refresh must never resurrect a removed account.
        self.save_registry(&registry)?;
        self.cleanup_credentials(id, None)
    }
    fn load_lease(&self, registry: &Registry, id: &str) -> Result<Lease> {
        let account = registry
            .accounts
            .iter()
            .find(|a| a.id == id)
            .context("Account was removed")?
            .clone();
        let credential: Credential = read_json(&self.credential_path(id, &account.generation)?)?
            .context("Account credential is missing; sign in again")?;
        if credential.generation != account.generation
            || credential.subject != account.subject
            || credential.account_id != account.workspace
        {
            bail!("Account credential changed incompletely; sign in again");
        }
        Ok(Lease {
            account,
            credential,
        })
    }
    pub fn lease(&self, id: &str) -> Result<Lease> {
        let _guard = self.registry_lock()?;
        self.load_lease(&self.read_registry()?, id)
    }
    /// `None` means the selected account was removed or replaced, not corrupt storage.
    pub fn lease_for_generation(&self, id: &str, generation: &str) -> Result<Option<Lease>> {
        let _guard = self.registry_lock()?;
        let registry = self.read_registry()?;
        if !registry
            .accounts
            .iter()
            .any(|account| account.id == id && account.generation == generation)
        {
            return Ok(None);
        }
        self.load_lease(&registry, id).map(Some)
    }
    pub fn select_at(
        &self,
        excluded: &HashSet<String>,
        time: u64,
    ) -> Result<Lease, SelectionError> {
        self.select_for_provider_at("codex", excluded, time)
    }
    pub fn select_for_provider_at(
        &self,
        provider: &str,
        excluded: &HashSet<String>,
        time: u64,
    ) -> Result<Lease, SelectionError> {
        self.select_inner(provider, excluded, time)
            .map_err(SelectionError::Storage)?
    }
    fn select_inner(
        &self,
        provider: &str,
        excluded: &HashSet<String>,
        time: u64,
    ) -> Result<Result<Lease, SelectionError>> {
        let _guard = self.registry_lock()?;
        let mut registry = self.read_registry()?;
        let mut dirty = false;
        for account in &mut registry.accounts {
            if matches!(account.status, AccountStatus::QuotaExhausted { reset_at } if reset_at <= time)
            {
                account.status = AccountStatus::Available;
                account.status_revision = account
                    .status_revision
                    .checked_add(1)
                    .context("Account status counter exhausted")?;
                dirty = true;
            }
        }
        let start = registry
            .active
            .get(provider)
            .and_then(|id| registry.accounts.iter().position(|a| &a.id == id))
            .unwrap_or(0);
        let length = registry.accounts.len();
        for offset in 0..length {
            let account = &registry.accounts[(start + offset) % length];
            if account.provider == provider
                && account.status == AccountStatus::Available
                && !excluded.contains(&account.id)
            {
                let id = account.id.clone();
                let lease = self.load_lease(&registry, &id)?;
                if registry.active.get(provider) != Some(&id) {
                    registry.active.insert(provider.into(), id);
                    dirty = true;
                }
                if dirty {
                    self.save_registry(&registry)?;
                }
                return Ok(Ok(lease));
            }
        }
        if dirty {
            self.save_registry(&registry)?;
        }
        let reset = registry
            .accounts
            .iter()
            .filter(|a| a.provider == provider)
            .filter_map(|a| match a.status {
                AccountStatus::QuotaExhausted { reset_at } => Some(reset_at),
                _ => None,
            })
            .min();
        Ok(Err(match reset {
            Some(reset) => SelectionError::Exhausted {
                retry_after: reset.saturating_sub(time).max(1),
            },
            None => SelectionError::NoAccounts,
        }))
    }
    pub fn mark_quota_at(
        &self,
        id: &str,
        generation: &str,
        reset_at: u64,
        time: u64,
    ) -> Result<()> {
        self.update_account(
            id,
            generation,
            |a| {
                let reset_at = match a.status {
                    AccountStatus::NeedsLogin => return,
                    AccountStatus::QuotaExhausted { reset_at: existing } => existing.max(reset_at),
                    AccountStatus::Available => reset_at,
                };
                a.status = AccountStatus::QuotaExhausted {
                    reset_at: reset_at.max(time.saturating_add(1)),
                }
            },
            true,
            time,
        )
    }
    pub fn mark_needs_login(&self, id: &str, generation: &str) -> Result<()> {
        self.update_account(
            id,
            generation,
            |a| a.status = AccountStatus::NeedsLogin,
            true,
            now(),
        )
    }
    pub fn record_usage(&self, id: &str, generation: &str, usage: serde_json::Value) -> Result<()> {
        self.record_usage_observed(
            id,
            generation,
            usage,
            UsageObservation {
                observed_at: Some(now()),
                source: UsageSource::Inference,
            },
        )
        .map(|_| ())
    }
    /// Telemetry never changes eligibility or the provider's active account.
    pub fn record_usage_observed(
        &self,
        id: &str,
        generation: &str,
        usage: serde_json::Value,
        observation: UsageObservation,
    ) -> Result<bool> {
        let _guard = self.registry_lock()?;
        let mut registry = self.read_registry()?;
        let Some(account) = registry
            .accounts
            .iter_mut()
            .find(|account| account.id == id && account.generation == generation)
        else {
            return Ok(false);
        };
        if let (Some(previous), Some(incoming)) = (
            account
                .usage_observation
                .as_ref()
                .and_then(|o| o.observed_at),
            observation.observed_at,
        ) && previous > incoming
        {
            return Ok(true);
        }
        account.usage = Some(usage);
        account.usage_observation = Some(observation);
        self.save_registry(&registry)?;
        Ok(true)
    }
    /// A live provider confirmation can recover quota, never authentication or newer status.
    /// The caller must have received authoritative ordinary-usage availability, not percentages.
    pub fn reconcile_refreshed_quota(&self, observed: &Account) -> Result<Option<AccountStatus>> {
        let _guard = self.registry_lock()?;
        let mut registry = self.read_registry()?;
        let Some(account) = registry.accounts.iter_mut().find(|account| {
            account.id == observed.id
                && account.generation == observed.generation
                && account.provider == observed.provider
        }) else {
            return Ok(None);
        };
        if account.status_revision == observed.status_revision
            && matches!(account.status, AccountStatus::QuotaExhausted { .. })
        {
            account.status = AccountStatus::Available;
            account.status_revision = account
                .status_revision
                .checked_add(1)
                .context("Account status counter exhausted")?;
            let status = account.status.clone();
            self.save_registry(&registry)?;
            return Ok(Some(status));
        }
        // Return the actual status even if a concurrent observation prevented recovery.
        Ok(Some(account.status.clone()))
    }
    /// Separate from OAuth locks: reset serialization must not block token publication.
    pub fn try_quota_reset_lock(&self, id: &str) -> Result<Option<File>> {
        validate_id(id)?;
        let file = private_open(
            &self
                .root
                .join("locks")
                .join(format!("{id}.quota-reset.lock")),
        )?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }
    pub fn reset_attempt(&self, id: &str) -> Result<Option<ResetAttempt>> {
        validate_id(id)?;
        let attempt: Option<ResetAttempt> =
            read_json(&self.root.join(format!("quota-reset-{id}.json")))?;
        if let Some(attempt) = &attempt {
            validate_id(&attempt.request_id)?;
            validate_id(&attempt.generation)?;
            if attempt.account_id != id {
                bail!("Reset journal account mismatch");
            }
        }
        Ok(attempt)
    }
    /// The caller holds the per-account reset lock, including while dispatch is uncertain.
    pub fn save_reset_attempt(&self, attempt: &ResetAttempt) -> Result<()> {
        validate_id(&attempt.account_id)?;
        validate_id(&attempt.generation)?;
        validate_id(&attempt.request_id)?;
        atomic_json(
            &self
                .root
                .join(format!("quota-reset-{}.json", attempt.account_id)),
            attempt,
        )
    }
    /// Only authoritative post-redemption availability can clear the captured quota state.
    pub fn reconcile_quota_reset(&self, attempt: &ResetAttempt, allowed: bool) -> Result<bool> {
        if !allowed
            || !matches!(
                &attempt.state,
                ResetAttemptState::Confirmed(outcome) if outcome.confirms_redemption()
            )
        {
            return Ok(false);
        }
        let _guard = self.registry_lock()?;
        let mut registry = self.read_registry()?;
        let Some(account) = registry.accounts.iter_mut().find(|account| {
            account.id == attempt.account_id
                && account.generation == attempt.generation
                && account.provider == attempt.provider
                && account.status_revision == attempt.status_revision
        }) else {
            return Ok(false);
        };
        match account.status {
            AccountStatus::NeedsLogin => return Ok(false),
            AccountStatus::Available => return Ok(true),
            AccountStatus::QuotaExhausted { .. } => {
                account.status = AccountStatus::Available;
                account.status_revision = account
                    .status_revision
                    .checked_add(1)
                    .context("Account status counter exhausted")?;
            }
        }
        self.save_registry(&registry)?;
        Ok(true)
    }
    fn update_account(
        &self,
        id: &str,
        generation: &str,
        update: impl FnOnce(&mut Account),
        advance: bool,
        time: u64,
    ) -> Result<()> {
        let _guard = self.registry_lock()?;
        let mut registry = self.read_registry()?;
        let Some(index) = registry
            .accounts
            .iter()
            .position(|a| a.id == id && a.generation == generation)
        else {
            return Ok(());
        };
        update(&mut registry.accounts[index]);
        if advance {
            registry.accounts[index].status_revision = registry.accounts[index]
                .status_revision
                .checked_add(1)
                .context("Account status counter exhausted")?;
        }
        let provider = registry.accounts[index].provider.clone();
        if advance && registry.active.get(&provider).map(String::as_str) == Some(id) {
            let length = registry.accounts.len();
            let next = (1..=length)
                .map(|offset| &registry.accounts[(index + offset) % length])
                .find(|a| {
                    a.provider == provider
                        && match a.status {
                            AccountStatus::Available => true,
                            AccountStatus::QuotaExhausted { reset_at } => reset_at <= time,
                            _ => false,
                        }
                })
                .map(|a| a.id.clone());
            match next {
                Some(next) => {
                    registry.active.insert(provider, next);
                }
                None => {
                    registry.active.remove(&provider);
                }
            }
        }
        self.save_registry(&registry)
    }
    pub fn finish_refresh(
        &self,
        id: &str,
        generation: &str,
        rejected_access: &str,
        refreshed: Credential,
    ) -> Result<Credential> {
        self.finish_refresh_inner(id, generation, rejected_access, refreshed, false)?
            .context("Account changed during refresh")
    }
    /// Publish a rotation only while the selected generation remains current.
    pub fn finish_refresh_for_generation(
        &self,
        id: &str,
        generation: &str,
        rejected_access: &str,
        refreshed: Credential,
    ) -> Result<Option<Credential>> {
        self.finish_refresh_inner(id, generation, rejected_access, refreshed, true)
    }
    fn finish_refresh_inner(
        &self,
        id: &str,
        generation: &str,
        rejected_access: &str,
        mut refreshed: Credential,
        require_generation: bool,
    ) -> Result<Option<Credential>> {
        let _guard = self.registry_lock()?;
        let registry = self.read_registry()?;
        if require_generation
            && !registry
                .accounts
                .iter()
                .any(|account| account.id == id && account.generation == generation)
        {
            return Ok(None);
        }
        let current = self.load_lease(&registry, id)?.credential;
        if current.generation != generation || current.access_token != rejected_access {
            return Ok(Some(current));
        }
        if current.subject != refreshed.subject || current.account_id != refreshed.account_id {
            bail!("Refreshed credential belongs to a different account");
        }
        refreshed.generation = current.generation;
        atomic_json(
            &self.credential_path(id, &refreshed.generation)?,
            &refreshed,
        )?;
        Ok(Some(refreshed))
    }
    pub fn reasoning_key(&self) -> Result<[u8; 32]> {
        let _guard = self.registry_lock()?;
        let path = self.root.join("reasoning.key");
        if path.exists() {
            let bytes = fs::read(path)?;
            return bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("Invalid reasoning key length"));
        }
        let key: [u8; 32] = rand::random();
        atomic_bytes(&path, &key)?;
        Ok(key)
    }
}
impl InferenceAccountRepository for Store {
    fn list(&self) -> Result<AccountCatalog> {
        let registry = Store::list(self)?;
        Ok(AccountCatalog {
            accounts: registry.accounts,
            active: registry.active,
        })
    }

    fn select_for_provider_at(
        &self,
        provider: &str,
        excluded: &HashSet<String>,
        time: u64,
    ) -> Result<Account, SelectionError> {
        Store::select_for_provider_at(self, provider, excluded, time).map(|lease| lease.account)
    }

    fn record_usage(&self, id: &str, generation: &str, usage: serde_json::Value) -> Result<()> {
        Store::record_usage(self, id, generation, usage)
    }

    fn mark_quota_at(&self, id: &str, generation: &str, reset_at: u64, time: u64) -> Result<()> {
        Store::mark_quota_at(self, id, generation, reset_at, time)
    }

    fn mark_needs_login(&self, id: &str, generation: &str) -> Result<()> {
        Store::mark_needs_login(self, id, generation)
    }
}

impl AccountCommandRepository for Store {
    type Ticket = LoginTicket;

    fn begin_login(&self, provider: &str) -> Result<LoginTicket> {
        Store::begin_login(self, provider)
    }

    fn finish_login(
        &self,
        ticket: LoginTicket,
        name: &str,
        credential: Credential,
    ) -> Result<Account> {
        Store::finish_login(self, ticket, name, credential)
    }

    fn remove(&self, id: &str) -> Result<()> {
        Store::remove(self, id)
    }
}

impl AccountUsageRepository for Store {
    fn list(&self) -> Result<AccountCatalog> {
        InferenceAccountRepository::list(self)
    }

    fn record_usage_observed(
        &self,
        id: &str,
        generation: &str,
        usage: serde_json::Value,
        observation: UsageObservation,
    ) -> Result<bool> {
        Store::record_usage_observed(self, id, generation, usage, observation)
    }

    fn reconcile_refreshed_quota(&self, observed: &Account) -> Result<Option<AccountStatus>> {
        Store::reconcile_refreshed_quota(self, observed)
    }

    fn reset_attempt(&self, id: &str) -> Result<Option<ResetAttempt>> {
        Store::reset_attempt(self, id)
    }

    fn save_reset_attempt(&self, attempt: &ResetAttempt) -> Result<()> {
        Store::save_reset_attempt(self, attempt)
    }

    fn reconcile_quota_reset(&self, attempt: &ResetAttempt, allowed: bool) -> Result<bool> {
        Store::reconcile_quota_reset(self, attempt, allowed)
    }

    fn try_quota_reset_lock(&self, id: &str) -> Result<Option<ResetGuard>> {
        Store::try_quota_reset_lock(self, id).map(|guard| guard.map(ResetGuard::new))
    }
}

fn validate_id(id: &str) -> Result<()> {
    Uuid::parse_str(id).context("Invalid account ID")?;
    Ok(())
}
fn next_operation(registry: &mut Registry) -> Result<u64> {
    registry.operation_sequence = registry
        .operation_sequence
        .checked_add(1)
        .context("Account operation counter exhausted")?;
    Ok(registry.operation_sequence)
}
fn identity_key(provider: &str, subject: &str, workspace: &str) -> Result<String> {
    Ok(serde_json::to_string(&(provider, subject, workspace))?)
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn private_open(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
fn lock_file(path: &Path) -> Result<File> {
    let file = private_open(path)?;
    file.lock()?;
    Ok(file)
}
fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        bail!("Account storage exceeds size limit");
    }
    Ok(Some(
        serde_json::from_slice(&bytes).context("Invalid account storage JSON")?,
    ))
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    atomic_bytes(path, &serde_json::to_vec_pretty(value)?)
}
fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Invalid state path")?;
    let temp = parent.join(format!(".tmp-{}", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
