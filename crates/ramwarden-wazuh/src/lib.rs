//! The Wazuh warden's mechanism: what this computer's Wazuh agent says about
//! itself, and what its Wazuh manager and indexer say about it.
//!
//! Like `ramwarden-kernel` and `ramwarden-nt`, this crate measures and never
//! decides. Turning findings into a level the user sees is
//! `ramwarden_core::wardens::assess_threats`.
//!
//! ## Why files and REST, and nothing linked
//!
//! Wazuh is GPLv2, and its newer engine AGPLv3. RamWarden links, vendors and
//! modifies none of it: it reads the agent's state file and calls the
//! documented server API (`:55000`) and indexer search API (`:9200`). The user
//! installs Wazuh's own unmodified packages. That separation is what lets the
//! Wazuh warden be a paid add-on without changing RamWarden's licence.
//!
//! ## Read-only by construction
//!
//! The clients expose authentication, agent lookup and alert search. There is
//! no method that calls active response, restarts an agent or changes a rule,
//! so no bug in policy can make the Wazuh warden act. The one thing that
//! changes the computer is [`install`], which installs Wazuh's own agent when
//! the user asks, behind the OS's own admin prompt.
//!
//! ## Hosted or self-hosted
//!
//! By default RamWarden hosts Wazuh and [`cloud`] talks to it with the device
//! sign-in token: no keys, no server, no certificates for the user. [`api`]
//! talks to a manager the user runs themselves.

pub mod agent;
pub mod api;
pub mod cloud;
pub mod install;

pub use agent::{LocalAgent, default_agent_name};
pub use api::{Alert, Client, Endpoints, ManagerAgent};
pub use cloud::{Cloud, CloudStatus, EnrollRequest, Enrollment};

/// Everything that can go wrong talking to Wazuh. Each is a normal
/// configuration the warden reports as "off", never a reason to fail.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A URL that would send a password in plain text off this machine.
    #[error("{0} must use https (plain http is allowed only on this computer)")]
    Insecure(String),
    #[error("bad URL {0}: {1}")]
    BadUrl(String, String),
    #[error("cannot read CA file {0}: {1}")]
    CaFile(String, String),
    #[error("the Wazuh {0} refused the credentials")]
    Auth(&'static str),
    #[error("the Wazuh {0} answered HTTP {1}")]
    Status(&'static str, u16),
    #[error("cannot reach the Wazuh {0}: {1}")]
    Unreachable(&'static str, String),
    #[error("unexpected answer from the Wazuh {0}: {1}")]
    Parse(&'static str, String),
    /// The device token was refused: sign in to RamWarden again.
    #[error("this computer is signed out of RamWarden")]
    SignedOut,
    /// The account has no current Wazuh pass.
    #[error("this account has no Wazuh warden pass")]
    NoPass,
    #[error("{0}")]
    Install(String),
}

pub type Result<T> = std::result::Result<T, Error>;
