//! Everything RamWarden needs the OS to allow, asked for once, at setup.
//!
//! # Why
//!
//! A prompt in the middle of use is the worst time to ask: the user is doing
//! something else, does not know why a memory manager wants Apple Music, and
//! learns to click "Don't Allow". On the reference Mac (2026-11-19) RamWarden
//! asked three times mid-use: for Documents and Downloads (the storage scan
//! walking the home folder), for Apple Music (the scan opening the media
//! library), and to control an app it was quitting (`osascript`).
//!
//! The rule now, on every OS:
//!
//! 1. **Nothing prompts after setup.** The daemon never opens a folder macOS
//!    guards (`storage::catalog::MAC_PRIVATE`) unless it already has Full Disk
//!    Access, and never sends Apple events (apps are asked to quit through
//!    `ramwarden-quit`, AppKit's own request).
//! 2. **Setup asks for all of it up front.** The setup app reads
//!    `GET /permissions` and shows each need, why, whether it is granted, and
//!    the one place to grant it. An optional one left ungranted is a smaller
//!    feature, never a later prompt.
//!
//! What is asked for, per OS:
//!
//! | OS | Need | Without it |
//! |---|---|---|
//! | macOS | Full Disk Access (optional) | Storage leaves Desktop, Documents, Downloads, media libraries and other apps' data out, and says so |
//! | macOS | `ramwarden-quit` installed | Idle apps are not asked to quit |
//! | Linux | Page-out helper (installed with one `sudo` at install) | Kernel reclaim per process is skipped; the rest of the ladder runs |
//! | Windows | RamWarden service (one UAC prompt at install) | Privileged trims and debloat are unavailable |

use std::path::Path;

use axum::{Json, Router, routing::get};
use serde::Serialize;
use serde_json::{Value, json};

/// One thing the OS has to allow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Need {
    pub id: &'static str,
    pub label: &'static str,
    /// What it is for, in one sentence.
    pub why: &'static str,
    /// What RamWarden does without it.
    pub without: &'static str,
    /// `None` when it cannot be checked from here.
    pub granted: Option<bool>,
    /// Without it RamWarden still runs; the feature is smaller.
    pub optional: bool,
    /// Where the user grants it: a settings URL the setup app opens.
    pub settings: Option<&'static str>,
    /// What to add there, when the grant names a program.
    pub program: Option<String>,
}

/// The macOS settings pane for Full Disk Access.
pub const MAC_FULL_DISK: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// What this computer needs. `checks` are the probes, passed in so tests do
/// not depend on the machine they run on.
pub fn needs(os: &str, checks: &Checks) -> Vec<Need> {
    match os {
        "macos" => vec![
            Need {
                id: "full_disk_access",
                label: "Full Disk Access",
                why: "Lets the storage warden measure Desktop, Documents, Downloads, your media libraries and other apps' data when the disk is filling up. Nothing is read for any other reason, and nothing leaves this Mac.",
                without: "Those folders are left out of storage scans and named as not measured. macOS never asks later.",
                granted: Some(checks.full_disk_access),
                optional: true,
                settings: Some(MAC_FULL_DISK),
                program: checks.daemon.clone(),
            },
            Need {
                id: "quit_helper",
                label: "Quit helper",
                why: "Asks idle apps you listed under [quit] to quit, the way their own Quit menu does, so they can still ask to save. It needs no permission from macOS.",
                without: "Idle apps are not asked to quit.",
                granted: Some(checks.quit_helper),
                optional: true,
                settings: None,
                program: None,
            },
        ],
        "linux" => vec![Need {
            id: "page_out_helper",
            label: "Page-out helper",
            why: "Moves idle memory of a busy app to zram or swap, which needs one capability the daemon does not hold. Installed once, with sudo, by the installer.",
            without: "Per-process reclaim is skipped; cgroup reclaim and the rest of the ladder still run.",
            granted: Some(checks.page_out_helper),
            optional: true,
            settings: None,
            program: None,
        }],
        "windows" => vec![Need {
            id: "service",
            label: "RamWarden service",
            why: "Trims working sets of other users' and elevated processes, and applies reversible debloat profiles. Installed once, with one administrator prompt, by the installer.",
            without: "Only your own processes are trimmed and debloat is unavailable.",
            granted: Some(checks.service),
            optional: true,
            settings: None,
            program: None,
        }],
        _ => Vec::new(),
    }
}

/// The probes behind [`needs`]. None of them prompts.
#[derive(Clone, Debug, Default)]
pub struct Checks {
    pub full_disk_access: bool,
    pub quit_helper: bool,
    pub page_out_helper: bool,
    pub service: bool,
    /// This daemon's executable, which is what Full Disk Access must name.
    pub daemon: Option<String>,
}

impl Checks {
    /// Probe this computer. Each probe only reads state it is allowed to read.
    pub fn here(home: &Path) -> Self {
        let os = std::env::consts::OS;
        let helper = crate::extras::helper();
        Checks {
            full_disk_access: os == "macos" && crate::storage::catalog::full_disk_access(home),
            quit_helper: os == "macos" && crate::appquit::quit_helper().is_some(),
            page_out_helper: helper["helper_present"].as_bool().unwrap_or(false),
            service: service_present(),
            daemon: std::env::current_exe().ok().map(|p| p.display().to_string()),
        }
    }
}

#[cfg(windows)]
fn service_present() -> bool {
    ramwarden_nt::pipe::present()
}

#[cfg(not(windows))]
fn service_present() -> bool {
    false
}

/// `GET /permissions`.
pub fn view(os: &str, needs: &[Need]) -> Value {
    json!({
        "os": os,
        "needs": needs,
        "all_granted": needs.iter().all(|n| n.granted != Some(false)),
        "note": "Asked for once, at setup. RamWarden never prompts for these, or anything else, later: a need left ungranted only makes its feature smaller.",
    })
}

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new().route(
        "/permissions",
        get(|| async {
            let os = std::env::consts::OS;
            let checks = tokio::task::spawn_blocking(|| Checks::here(&ramwarden_core::config::home())).await.unwrap_or_default();
            Json(view(os, &needs(os, &checks)))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_os_lists_what_it_needs_and_where_to_grant_it() {
        let none = Checks { daemon: Some("/usr/local/bin/ramwarden-daemon".into()), ..Default::default() };
        let mac = needs("macos", &none);
        let fda = mac.iter().find(|n| n.id == "full_disk_access").unwrap();
        assert_eq!(fda.granted, Some(false));
        assert_eq!(fda.settings, Some(MAC_FULL_DISK));
        assert_eq!(fda.program.as_deref(), Some("/usr/local/bin/ramwarden-daemon"));
        assert!(mac.iter().all(|n| n.optional), "nothing on the Mac is required to run");
        assert_eq!(needs("linux", &none)[0].id, "page_out_helper");
        assert_eq!(needs("windows", &none)[0].id, "service");
        assert!(needs("plan9", &none).is_empty());
    }

    #[test]
    fn the_view_says_whether_everything_is_granted() {
        let all = Checks { full_disk_access: true, quit_helper: true, ..Default::default() };
        assert_eq!(view("macos", &needs("macos", &all))["all_granted"], true);
        let some = Checks { quit_helper: true, ..Default::default() };
        let v = view("macos", &needs("macos", &some));
        assert_eq!(v["all_granted"], false);
        assert_eq!(v["needs"][0]["id"], "full_disk_access");
        assert!(v["note"].as_str().unwrap().contains("never prompts"));
    }
}
