//! Account identity, private persistence, snapshots, and sticky account selection.
mod store;
pub use store::{
    Account, AccountStatus, Credential, Lease, LoginTicket, Registry, SelectionError, Store, now,
};
