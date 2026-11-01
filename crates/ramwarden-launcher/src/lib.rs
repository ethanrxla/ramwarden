//! The launcher: installs, updates and reports on RamWarden's wardens.
//!
//! A *warden* is one agent the user can run: the RAM warden (the daemon, the
//! tray or menu-bar app, the extension bridge) ships free; the Wazuh threat
//! warden and later ones are paid add-ons. The launcher knows nothing about any
//! of them beyond what the signed release manifest says: its name, tier, icon,
//! version and one installer per platform. Adding a warden is a release change,
//! not a launcher change.
//!
//! # Trust chain
//!
//! ```text
//! update-keys.txt (compiled in) ──verifies──▶ manifest.json (+ .sig)
//! manifest.json   ──sha256 + size──▶ each installer it lists
//! ```
//!
//! The manifest is fetched over HTTPS but HTTPS is not what we trust: the
//! Ed25519 signature is. The installers are then trusted only by the hashes in
//! that signed manifest, so a compromised download host (GitHub releases, the
//! paid-download API) can withhold an update but never substitute one. The
//! manifest carries a `sequence` that only rises; the launcher remembers the
//! highest it has accepted and refuses an older one, so an attacker cannot
//! replay a genuine but outdated manifest to pin users on a vulnerable build.
//!
//! # What runs the installer
//!
//! The launcher does not copy files into place itself. Each platform already
//! has an installer that knows how (`install.ps1` behind an Inno Setup exe, a
//! `.deb`, `install.sh` in a tarball); the launcher verifies it and runs it,
//! with elevation where the platform needs it. One install path per platform
//! means a fresh install and an update can never disagree.

pub mod fetch;
pub mod install;
pub mod link;
pub mod manifest;
pub mod release;
pub mod signing;
pub mod state;
pub mod target;
pub mod updater;

pub use manifest::{Artifact, Format, Manifest, Tier, WardenRelease};
pub use signing::Verifier;
pub use updater::{Applied, Available, Updater};

/// Everything that can go wrong. Each variant names what the user can do.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("this build has no update key, so it cannot verify updates")]
    NoKeys,
    #[error("the release manifest's signature does not verify")]
    BadSignature,
    #[error("the release manifest is older than one already seen (sequence {got} < {seen})")]
    Replayed { got: u64, seen: u64 },
    #[error("the release manifest is malformed: {0}")]
    Manifest(String),
    #[error("{file}: expected {expected}, downloaded {actual}")]
    Mismatch {
        file: String,
        expected: String,
        actual: String,
    },
    #[error("{0} needs a Pro pass; sign in from the app first")]
    NeedsPro(String),
    #[error("no {warden} build for {target}")]
    NoArtifact { warden: String, target: String },
    #[error("download failed: {0}")]
    Fetch(String),
    #[error("installer failed: {0}")]
    Install(String),
    #[error("{0}")]
    Unsupported(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
