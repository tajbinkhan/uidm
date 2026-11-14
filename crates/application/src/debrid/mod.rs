//! A debrid account (experimental): links to file hosts the account
//! supports are unlocked into direct links before they download, the
//! account can be checked from Options, and its key stored or removed.

mod check_account;
mod resolve_link;
mod set_key;

pub use check_account::CheckAccount;
pub use resolve_link::{ResolveLink, HOSTS_FOR, LINK_FOR};
pub use set_key::SetKey;
pub use udm_domain::debrid::Refused;
