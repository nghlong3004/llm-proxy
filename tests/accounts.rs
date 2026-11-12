use llm_proxy::accounts::{Credential, SelectionError, Store};
use std::collections::HashSet;

fn credential(subject: &str, workspace: &str) -> Credential {
    Credential {
        access_token: format!("access-{subject}"),
        refresh_token: format!("refresh-{subject}"),
        id_token: None,
        expires_at: u64::MAX,
        subject: subject.into(),
        account_id: workspace.into(),
        generation: String::new(),
    }
}

#[test]
fn users_in_same_workspace_are_distinct_and_relogin_updates_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = store.login("one", credential("user-a", "team")).unwrap();
    let b = store.login("two", credential("user-b", "team")).unwrap();
    assert_ne!(a.id, b.id);
    let again = store
        .login("renamed", credential("user-a", "team"))
        .unwrap();
    assert_eq!(a.id, again.id);
    assert_ne!(a.generation, again.generation);
    assert_eq!(store.list().unwrap().accounts.len(), 2);
}

#[test]
fn quota_failover_is_sticky_and_reset_does_not_steal_active_account() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = store.login("one", credential("a", "w")).unwrap();
    let b = store.login("two", credential("b", "w")).unwrap();
    let c = store.login("three", credential("c", "w")).unwrap();
    assert_eq!(
        store.select_at(&HashSet::new(), 100).unwrap().account.id,
        a.id
    );
    store.mark_quota_at(&a.id, &a.generation, 200, 100).unwrap();
    assert_eq!(
        store.select_at(&HashSet::new(), 100).unwrap().account.id,
        b.id
    );
    assert_eq!(
        store.select_at(&HashSet::new(), 201).unwrap().account.id,
        b.id
    );
    store.mark_quota_at(&b.id, &b.generation, 300, 201).unwrap();
    assert_eq!(
        store.select_at(&HashSet::new(), 201).unwrap().account.id,
        c.id
    );
}

#[test]
fn all_exhausted_returns_earliest_eligible_reset() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = store.login("one", credential("a", "w")).unwrap();
    let b = store.login("two", credential("b", "w")).unwrap();
    store.mark_quota_at(&a.id, &a.generation, 160, 100).unwrap();
    store.mark_quota_at(&b.id, &b.generation, 200, 100).unwrap();
    assert!(matches!(
        store.select_at(&HashSet::new(), 100),
        Err(SelectionError::Exhausted { retry_after: 60 })
    ));
}

#[test]
fn stale_refresh_and_quota_cannot_overwrite_relogin_or_resurrect_removal() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = store.login("one", credential("a", "w")).unwrap();
    let new = store.login("new", credential("a", "w")).unwrap();
    store.mark_quota_at(&a.id, &a.generation, 500, 100).unwrap();
    assert_eq!(
        store
            .select_at(&HashSet::new(), 100)
            .unwrap()
            .account
            .generation,
        new.generation
    );
    let mut stale = credential("a", "w");
    stale.access_token = "stale-refresh".into();
    let current = store
        .finish_refresh(&a.id, &a.generation, "access-a", stale)
        .unwrap();
    assert_eq!(current.generation, new.generation);
    store.remove(&a.id).unwrap();
    assert!(
        store
            .finish_refresh(&a.id, &a.generation, "access-a", credential("a", "w"))
            .is_err()
    );
    assert!(store.list().unwrap().accounts.is_empty());
}

#[test]
fn separate_store_handles_see_hot_changes_and_use_private_files() {
    let dir = tempfile::tempdir().unwrap();
    let one = Store::open(dir.path()).unwrap();
    let two = Store::open(dir.path()).unwrap();
    let a = one.login("one", credential("a", "w")).unwrap();
    assert_eq!(
        two.select_at(&HashSet::new(), 100).unwrap().account.id,
        a.id
    );
    two.remove(&a.id).unwrap();
    assert!(matches!(
        one.select_at(&HashSet::new(), 100),
        Err(SelectionError::NoAccounts)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("accounts.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn account_selection_is_scoped_to_provider_and_concurrent_adds_are_not_lost() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let other = store
        .login_for("other", "other", credential("a", "w"))
        .unwrap();
    assert!(matches!(
        store.select_at(&HashSet::new(), 100),
        Err(SelectionError::NoAccounts)
    ));
    let mut threads = vec![];
    for i in 0..12 {
        let store = store.clone();
        threads.push(std::thread::spawn(move || {
            store
                .login("parallel", credential(&format!("u{i}"), "w"))
                .unwrap()
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(store.list().unwrap().accounts.len(), 13);
    assert_ne!(
        store.select_at(&HashSet::new(), 100).unwrap().account.id,
        other.id
    );
    assert_eq!(
        store
            .select_for_provider_at("other", &HashSet::new(), 100)
            .unwrap()
            .account
            .id,
        other.id
    );
}

#[test]
fn refresh_lock_serializes_separate_store_handles() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = store.login("one", credential("a", "w")).unwrap();
    let first = store.refresh_lock(&a.id).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let _second = store.refresh_lock(&a.id).unwrap();
        tx.send(()).unwrap();
    });
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(30))
            .is_err()
    );
    drop(first);
    rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    thread.join().unwrap();
}

#[test]
fn older_oauth_completion_cannot_overwrite_newer_login_or_resurrect_removal() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let first = store.login("original", credential("a", "w")).unwrap();
    let old = store.begin_login("codex").unwrap();
    let new = store.begin_login("codex").unwrap();
    let current = store
        .finish_login(new, "newer", credential("a", "w"))
        .unwrap();
    assert!(
        store
            .finish_login(old, "older", credential("a", "w"))
            .is_err()
    );
    assert_eq!(
        store.lease(&first.id).unwrap().account.generation,
        current.generation
    );
    let pending = store.begin_login("codex").unwrap();
    store.remove(&first.id).unwrap();
    assert!(
        store
            .finish_login(pending, "removed", credential("a", "w"))
            .is_err()
    );
    assert!(store.list().unwrap().accounts.is_empty());
    // A fresh, explicit login after removal may add that identity again.
    assert!(store.login("again", credential("a", "w")).is_ok());
}

#[test]
fn concurrent_quota_reports_preserve_longest_reset_and_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let a = store.login("a", credential("a", "w")).unwrap();
    store.mark_quota_at(&a.id, &a.generation, 500, 100).unwrap();
    store.mark_quota_at(&a.id, &a.generation, 200, 100).unwrap();
    assert!(matches!(
        store.select_at(&HashSet::new(), 100),
        Err(SelectionError::Exhausted { retry_after: 400 })
    ));
    store.mark_needs_login(&a.id, &a.generation).unwrap();
    store.mark_quota_at(&a.id, &a.generation, 600, 100).unwrap();
    assert!(matches!(
        store.select_at(&HashSet::new(), 100),
        Err(SelectionError::NoAccounts)
    ));
}
