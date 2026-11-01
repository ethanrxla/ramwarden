//! Claiming a personal install line's link token on first run.
//!
//! The site's one-liner (`curl … /v1/install/<token>/sh | sh`, or `irm … | iex`
//! on Windows) saves a device code, approved for the buyer's account when it
//! was issued, to `<config dir>/ramwarden/link-token`, then runs the installer.
//! Whatever runs first afterwards (the installer's own last step, or the
//! scheduled update check) trades it for a device token through the ordinary
//! `POST /v1/device/token`, naming this machine. The device token goes to
//! `<config dir>/ramwarden/device-token`, where the daemon
//! ([`ramwarden_core::config::device_token`]) and the launcher read it, and the
//! link token is deleted.
//!
//! The code is single use and lasts 30 minutes, so the file is deleted on
//! every answer that makes it useless (claimed, expired, account full) and kept
//! only when no answer came (offline: the next run tries again). A link that
//! cannot be claimed is not an error the user must handle: the app falls back
//! to the device sign-in page as it would without one.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::fetch::Fetch;
use crate::{Error, Result};

const LINK: &str = "link-token";
const DEVICE: &str = "device-token";

/// What a claim attempt did. Reported as is, like every launcher outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Linked {
    /// No link token on this machine.
    Nothing,
    /// Signed in: the device token is saved and the link token deleted.
    Claimed { account_id: String, device_id: String },
    /// A device token was already there; the link token was deleted unused,
    /// so one install line never puts a machine on an account twice.
    AlreadySignedIn,
    /// The server no longer knows the code (used, or past 30 minutes).
    Expired,
    /// The account has no free device slot.
    DeviceLimit,
    /// The code is not approved yet. Install lines are approved when issued,
    /// so this is unexpected; the token is kept for the next run.
    Pending,
}

pub struct Link {
    /// `<config dir>/ramwarden`: where the device token is written.
    pub dir: PathBuf,
    /// Further places an install script may have left the link token.
    pub also: Vec<PathBuf>,
    /// The hosted API, from `[updates].api_url`.
    pub api_url: String,
}

#[derive(Deserialize)]
struct Claimed {
    device_token: String,
    #[serde(default)]
    account_id: String,
    #[serde(default)]
    device_id: String,
}

#[derive(Deserialize)]
struct ApiError {
    error: String,
}

impl Link {
    /// The locations RamWarden uses on this machine. On macOS the install
    /// script of the first release wrote to `~/Library/Application Support`,
    /// while the daemon's config lives in `~/.config`; both are checked so a
    /// link from either works.
    pub fn for_user(api_url: &str) -> Link {
        let dir = ramwarden_core::config::config_dir().join("ramwarden");
        let mut also = Vec::new();
        if cfg!(target_os = "macos")
            && let Some(home) = std::env::var_os("HOME")
        {
            also.push(
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
                    .join("ramwarden"),
            );
        }
        Link {
            dir,
            also,
            api_url: api_url.trim_end_matches('/').to_string(),
        }
    }

    /// Every existing link-token file, the canonical one first.
    fn link_files(&self) -> Vec<PathBuf> {
        std::iter::once(&self.dir)
            .chain(&self.also)
            .map(|d| d.join(LINK))
            .filter(|p| p.is_file())
            .collect()
    }

    fn forget(files: &[PathBuf]) {
        for f in files {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Claim the link token, if there is one.
    pub fn claim<F: Fetch>(&self, fetch: &F, device_name: &str, platform: &str) -> Result<Linked> {
        let files = self.link_files();
        let Some(first) = files.first() else {
            return Ok(Linked::Nothing);
        };
        if self.dir.join(DEVICE).is_file() {
            Self::forget(&files);
            return Ok(Linked::AlreadySignedIn);
        }
        let code = std::fs::read_to_string(first)?.trim().to_string();
        if !plausible(&code) {
            // Nothing the server issued: not worth a request, never valid later.
            Self::forget(&files);
            return Ok(Linked::Expired);
        }
        let body = serde_json::json!({
            "device_code": code,
            "device_name": device_name,
            "platform": platform,
        });
        let (status, bytes) = fetch.post_json(&format!("{}/v1/device/token", self.api_url), &body)?;
        let api_error = || {
            serde_json::from_slice::<ApiError>(&bytes)
                .map(|e| e.error)
                .unwrap_or_default()
        };
        let linked = match status {
            200..=299 => {
                let c: Claimed = serde_json::from_slice(&bytes)
                    .map_err(|e| Error::Fetch(format!("device token reply: {e}")))?;
                if !plausible(&c.device_token) {
                    return Err(Error::Fetch("device token reply has no usable token".into()));
                }
                write_private(&self.dir.join(DEVICE), &c.device_token)?;
                Linked::Claimed {
                    account_id: c.account_id,
                    device_id: c.device_id,
                }
            }
            400 if api_error() == "authorization_pending" => return Ok(Linked::Pending),
            400 if api_error() == "expired_token" => Linked::Expired,
            403 if api_error() == "device_limit" => Linked::DeviceLimit,
            // A 5xx, a proxy page, anything unexpected: keep the token and
            // let the next run try, within the code's 30 minutes.
            other => {
                return Err(Error::Fetch(format!(
                    "device token: HTTP {other} {}",
                    api_error()
                )));
            }
        };
        Self::forget(&files);
        Ok(linked)
    }
}

/// Tokens are opaque but printable: letters, digits, `_`, `-`, `.`, at most
/// 512 bytes. Anything else is refused before it is sent or saved.
fn plausible(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 512
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
}

/// Write a secret readable only by its owner, replacing any earlier one
/// atomically. On Windows the per-user `%APPDATA%` already has that ACL.
fn write_private(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().ok_or_else(|| Error::Unsupported("no config directory".into()))?;
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    {
        use std::io::Write;
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The name this machine shows on the account page.
pub fn device_name() -> String {
    let raw = host_name().unwrap_or_default();
    let name = raw.trim().trim_end_matches(".local").trim();
    if name.is_empty() {
        format!("{} computer", platform_label())
    } else {
        name.chars().filter(|c| !c.is_control()).take(64).collect()
    }
}

#[cfg(unix)]
fn host_name() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length; gethostname NUL-terminates
    // within it on success (and we cap at the first NUL regardless).
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

#[cfg(not(unix))]
fn host_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

/// The API's `platform` value for this build.
pub fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    }
}

fn platform_label() -> &'static str {
    match platform() {
        "macos" => "Mac",
        "windows" => "Windows",
        _ => "Linux",
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io::Write;

    use super::*;

    /// Answers every POST with one fixed reply and records what was sent.
    struct Api {
        reply: Result<(u16, Vec<u8>)>,
        sent: RefCell<Vec<(String, serde_json::Value)>>,
    }

    impl Api {
        fn new(status: u16, body: &str) -> Api {
            Api {
                reply: Ok((status, body.as_bytes().to_vec())),
                sent: RefCell::default(),
            }
        }
        fn offline() -> Api {
            Api {
                reply: Err(Error::Fetch("offline".into())),
                sent: RefCell::default(),
            }
        }
    }

    impl Fetch for Api {
        fn fetch(&self, _: &str, _: Option<&str>, _: u64, _: &mut dyn Write) -> Result<u64> {
            unreachable!("claims only POST")
        }
        fn healthy(&self, _: &str) -> bool {
            false
        }
        fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<(u16, Vec<u8>)> {
            self.sent.borrow_mut().push((url.to_string(), body.clone()));
            match &self.reply {
                Ok(r) => Ok(r.clone()),
                Err(e) => Err(Error::Fetch(e.to_string())),
            }
        }
    }

    struct Rig {
        link: Link,
        _t: tempfile::TempDir,
    }

    fn rig(with_link: Option<&str>) -> Rig {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("ramwarden");
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(code) = with_link {
            std::fs::write(dir.join(LINK), format!("{code}\n")).unwrap();
        }
        Rig {
            link: Link {
                dir,
                also: vec![t.path().join("appsupport")],
                api_url: "https://api.example.test".into(),
            },
            _t: t,
        }
    }

    impl Rig {
        fn has(&self, name: &str) -> bool {
            self.link.dir.join(name).exists()
        }
    }

    #[test]
    fn no_link_token_sends_nothing() {
        let r = rig(None);
        let api = Api::new(200, "{}");
        assert_eq!(r.link.claim(&api, "box", "linux").unwrap(), Linked::Nothing);
        assert!(api.sent.borrow().is_empty());
    }

    #[test]
    fn a_claim_saves_the_device_token_privately_and_deletes_the_link() {
        let r = rig(Some("rwdc_abc123"));
        let api = Api::new(200, r#"{"device_token":"rwd_tok-1","account_id":"a1","device_id":"d1"}"#);
        let got = r.link.claim(&api, "Ethan's PC", "windows").unwrap();
        assert_eq!(got, Linked::Claimed { account_id: "a1".into(), device_id: "d1".into() });
        let sent = api.sent.borrow();
        assert_eq!(sent[0].0, "https://api.example.test/v1/device/token");
        assert_eq!(
            sent[0].1,
            serde_json::json!({"device_code": "rwdc_abc123", "device_name": "Ethan's PC", "platform": "windows"})
        );
        assert_eq!(std::fs::read_to_string(r.link.dir.join(DEVICE)).unwrap(), "rwd_tok-1");
        assert!(!r.has(LINK));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(r.link.dir.join(DEVICE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn dead_codes_are_deleted_and_reported() {
        for (status, err, want) in [
            (400, "expired_token", Linked::Expired),
            (403, "device_limit", Linked::DeviceLimit),
        ] {
            let r = rig(Some("rwdc_x"));
            let api = Api::new(status, &format!(r#"{{"error":"{err}"}}"#));
            assert_eq!(r.link.claim(&api, "box", "linux").unwrap(), want);
            assert!(!r.has(LINK) && !r.has(DEVICE));
        }
    }

    #[test]
    fn no_answer_or_a_server_error_keeps_the_link_for_the_next_run() {
        let r = rig(Some("rwdc_x"));
        assert!(r.link.claim(&Api::offline(), "box", "linux").is_err());
        assert!(r.has(LINK));
        assert!(r.link.claim(&Api::new(502, "<html>bad gateway</html>"), "box", "linux").is_err());
        assert!(r.has(LINK));
        let pending = Api::new(400, r#"{"error":"authorization_pending"}"#);
        assert_eq!(r.link.claim(&pending, "box", "linux").unwrap(), Linked::Pending);
        assert!(r.has(LINK));
    }

    #[test]
    fn an_existing_sign_in_is_kept_and_the_link_dropped_unused() {
        let r = rig(Some("rwdc_x"));
        std::fs::write(r.link.dir.join(DEVICE), "rwd_old").unwrap();
        let api = Api::new(200, r#"{"device_token":"rwd_new"}"#);
        assert_eq!(r.link.claim(&api, "box", "linux").unwrap(), Linked::AlreadySignedIn);
        assert!(api.sent.borrow().is_empty());
        assert_eq!(std::fs::read_to_string(r.link.dir.join(DEVICE)).unwrap(), "rwd_old");
        assert!(!r.has(LINK));
    }

    #[test]
    fn a_link_left_in_the_other_macos_folder_is_found_and_cleared() {
        let r = rig(None);
        let other = r.link.also[0].clone();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join(LINK), "rwdc_mac").unwrap();
        let api = Api::new(200, r#"{"device_token":"rwd_mac"}"#);
        assert!(matches!(r.link.claim(&api, "mac", "macos").unwrap(), Linked::Claimed { .. }));
        assert_eq!(api.sent.borrow()[0].1["device_code"], "rwdc_mac");
        assert!(r.has(DEVICE), "saved where the daemon reads it");
        assert!(!other.join(LINK).exists());
    }

    #[test]
    fn junk_is_neither_sent_nor_saved() {
        let r = rig(Some("not a token; rm -rf /"));
        let api = Api::new(200, r#"{"device_token":"x"}"#);
        assert_eq!(r.link.claim(&api, "box", "linux").unwrap(), Linked::Expired);
        assert!(api.sent.borrow().is_empty());

        let r = rig(Some("rwdc_ok"));
        let api = Api::new(200, r#"{"device_token":"bad token\n"}"#);
        assert!(r.link.claim(&api, "box", "linux").is_err());
        assert!(!r.has(DEVICE));
        assert!(r.has(LINK), "kept: the server may answer properly next time");
    }

    #[test]
    fn this_machine_has_a_name_and_a_platform() {
        assert!(!device_name().is_empty());
        assert!(["macos", "windows", "linux"].contains(&platform()));
    }
}
