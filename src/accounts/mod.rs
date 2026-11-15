//! Account identity, private persistence, snapshots, and sticky account selection.
mod store;
pub use crate::domain::{
    account::{AccountSnapshot as Account, AccountStatus, Credential, LoginTicket, SelectionError},
    time::now,
};
pub use store::{Lease, Registry, Store};
