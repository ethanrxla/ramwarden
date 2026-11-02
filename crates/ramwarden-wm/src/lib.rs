//! Tiling window manager backends: the mechanism behind
//! `docs/specs/desktop-arrange.md`.
//!
//! | OS | Backend | CLI |
//! |---|---|---|
//! | macOS | [`AeroSpace`] | `aerospace` |
//! | Linux | [`Ipc3`] (sway, i3) | `swaymsg`, `i3-msg` |
//! | Windows | [`Komorebi`] | `komorebic` |
//!
//! Each translates [`WmCommand`](ramwarden_platform::WmCommand) into its CLI
//! and parses that CLI's state back into
//! [`WmState`](ramwarden_platform::WmState). None of them decides whether a
//! command should run: validation, approval and verification are policy, in
//! `ramwarden_core::arrange`.
//!
//! Every backend is a CLI, so all three compile and are tested everywhere.
//! [`detect`] tries the ones that make sense on this OS, in order.

mod aerospace;
mod backup;
mod ipc3;
mod komorebi;
mod runner;

use std::sync::Arc;

pub use aerospace::AeroSpace;
pub use backup::ConfigBackups;
pub use ipc3::Ipc3;
pub use komorebi::Komorebi;
use ramwarden_platform::{WindowManager, WmKind};
pub use runner::{Runner, SystemRunner};

/// Backends worth trying on this OS, most likely first.
pub fn candidates() -> &'static [WmKind] {
    if cfg!(target_os = "macos") {
        &[WmKind::AeroSpace]
    } else if cfg!(windows) {
        &[WmKind::Komorebi]
    } else if std::env::var_os("SWAYSOCK").is_some() {
        &[WmKind::Sway, WmKind::I3]
    } else {
        &[WmKind::I3, WmKind::Sway]
    }
}

/// A backend for `kind`, without checking it is running.
pub fn backend(kind: WmKind, runner: Arc<dyn Runner>) -> Box<dyn WindowManager> {
    match kind {
        WmKind::AeroSpace => Box::new(AeroSpace::new(runner)),
        WmKind::Sway => Box::new(Ipc3::sway(runner)),
        WmKind::I3 => Box::new(Ipc3::i3(runner)),
        WmKind::Komorebi => Box::new(Komorebi::new(runner)),
    }
}

/// The first window manager that answers, or `None` when none is running.
///
/// `only` pins one backend (`[desktop] backend = "sway"`); `None` is auto.
/// Answering means its state query succeeded: an installed CLI whose window
/// manager is not running does not count.
pub fn detect(only: Option<WmKind>) -> Option<Box<dyn WindowManager>> {
    let runner: Arc<dyn Runner> = Arc::new(SystemRunner);
    let kinds: Vec<WmKind> = match only {
        Some(k) => vec![k],
        None => candidates().to_vec(),
    };
    for kind in kinds {
        let mut wm = backend(kind, runner.clone());
        match wm.state() {
            Ok(_) => {
                tracing::info!("window manager: {}", kind.as_str());
                return Some(wm);
            }
            Err(e) => tracing::debug!("{} not available: {e}", kind.as_str()),
        }
    }
    None
}
