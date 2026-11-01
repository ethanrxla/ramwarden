//! [`Secret`] moved to `ramwarden-platform` so mechanism crates that hold a
//! password (the Wazuh client) can use the same type without depending on the
//! model tier. Re-exported here so existing paths keep working.

pub use ramwarden_platform::secret::Secret;
