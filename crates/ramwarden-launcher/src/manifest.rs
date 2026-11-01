//! The release manifest: what each warden's latest version is and where its
//! installers are.
//!
//! ```json
//! {
//!   "schema": 1,
//!   "sequence": 7,
//!   "published_at": "2026-11-01T09:00:00Z",
//!   "wardens": [{
//!     "id": "ram", "name": "RAM Warden", "tier": "free", "version": "0.3.0",
//!     "icon": "https://ramwarden.app/icons/ram.svg",
//!     "summary": "Watches memory pressure and reclaims cheapest-first.",
//!     "health": "http://127.0.0.1:7823/health",
//!     "artifacts": [{
//!       "target": "windows-x86_64", "format": "exe",
//!       "url": "https://github.com/.../ramwarden-setup-0.3.0-x64.exe",
//!       "sha256": "…", "size": 12345678
//!     }]
//!   }]
//! }
//! ```
//!
//! The file is the whole of the launcher's knowledge about wardens, so it is
//! validated hard on parse: an artifact URL that is not HTTPS, a hash that is
//! not 64 hex digits or a duplicate warden id is a malformed manifest, not a
//! warden to skip.

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The only schema this launcher understands. A higher one means the launcher
/// itself is too old; it says so rather than guessing at new fields.
pub const SCHEMA: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub schema: u32,
    /// Rises with every published manifest. See the replay note in `lib.rs`.
    pub sequence: u64,
    pub published_at: String,
    pub wardens: Vec<WardenRelease>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Free,
    Pro,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WardenRelease {
    /// Stable id: `ram`, `wazuh`. Never reused for a different warden.
    pub id: String,
    pub name: String,
    pub tier: Tier,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// A loopback URL that answers 2xx while the warden runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    pub artifacts: Vec<Artifact>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Format {
    /// Windows: Inno Setup installer, run elevated and silent.
    #[serde(rename = "exe")]
    Exe,
    /// Debian and Ubuntu: installed with `apt-get` through `pkexec`.
    #[serde(rename = "deb")]
    Deb,
    /// macOS and other Linux: unpacked and its `install.sh` run as the user.
    #[serde(rename = "tar.gz")]
    TarGz,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artifact {
    pub target: String,
    pub format: Format,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

impl Artifact {
    /// The file name the launcher saves it under: the URL's last segment,
    /// refused if it could climb out of the staging directory.
    pub fn file_name(&self) -> Result<String> {
        let name = self
            .url
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .split(['?', '#'])
            .next()
            .unwrap_or_default();
        let safe = !name.is_empty()
            && name != "."
            && name != ".."
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-+".contains(c));
        if safe {
            Ok(name.to_string())
        } else {
            Err(Error::Manifest(format!("unusable file name in {}", self.url)))
        }
    }
}

impl Manifest {
    /// Parse and validate. Bytes must already have passed [`crate::Verifier`].
    pub fn parse(bytes: &[u8]) -> Result<Manifest> {
        let m: Manifest =
            serde_json::from_slice(bytes).map_err(|e| Error::Manifest(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA {
            return Err(Error::Manifest(format!(
                "schema {} (this launcher reads {SCHEMA}; update the launcher by reinstalling)",
                self.schema
            )));
        }
        let mut ids = std::collections::HashSet::new();
        for w in &self.wardens {
            if !ids.insert(w.id.as_str()) {
                return Err(Error::Manifest(format!("warden {} listed twice", w.id)));
            }
            if w.id.is_empty() || !w.id.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                return Err(Error::Manifest(format!("bad warden id {:?}", w.id)));
            }
            semver::Version::parse(&w.version)
                .map_err(|e| Error::Manifest(format!("{} version {}: {e}", w.id, w.version)))?;
            if let Some(h) = &w.health
                && !is_loopback_url(h)
            {
                return Err(Error::Manifest(format!("{} health URL is not loopback", w.id)));
            }
            for a in &w.artifacts {
                if !a.url.starts_with("https://") {
                    return Err(Error::Manifest(format!("{} is not HTTPS", a.url)));
                }
                if a.sha256.len() != 64 || !a.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(Error::Manifest(format!("bad sha256 for {}", a.url)));
                }
                a.file_name()?;
            }
        }
        Ok(())
    }

    pub fn warden(&self, id: &str) -> Option<&WardenRelease> {
        self.wardens.iter().find(|w| w.id == id)
    }
}

impl WardenRelease {
    pub fn semver(&self) -> semver::Version {
        // Validated in `Manifest::validate`.
        semver::Version::parse(&self.version).unwrap_or(semver::Version::new(0, 0, 0))
    }

    /// The artifact for `target` in the first of `formats` that has one.
    pub fn artifact_for(&self, target: &str, formats: &[Format]) -> Option<&Artifact> {
        formats.iter().find_map(|f| {
            self.artifacts
                .iter()
                .find(|a| a.target == target && a.format == *f)
        })
    }
}

/// `http://127.0.0.1:…`, `http://localhost:…` or `http://[::1]:…`. Health
/// checks must never make the launcher contact another host.
pub fn is_loopback_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', '?']).next().unwrap_or_default();
    let host = match host.rsplit_once(':') {
        Some((h, port)) if !h.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host,
    };
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn sample() -> Manifest {
        Manifest {
            schema: SCHEMA,
            sequence: 3,
            published_at: "2026-11-01T09:00:00Z".into(),
            wardens: vec![
                WardenRelease {
                    id: "ram".into(),
                    name: "RAM Warden".into(),
                    tier: Tier::Free,
                    version: "0.3.0".into(),
                    icon: None,
                    summary: None,
                    health: Some("http://127.0.0.1:7823/health".into()),
                    artifacts: vec![
                        artifact("linux-x86_64", Format::Deb, "ramwarden_0.3.0_amd64.deb"),
                        artifact("linux-x86_64", Format::TarGz, "ramwarden-0.3.0-linux-x86_64.tar.gz"),
                    ],
                },
                WardenRelease {
                    id: "wazuh".into(),
                    name: "Wazuh Warden".into(),
                    tier: Tier::Pro,
                    version: "0.1.0".into(),
                    icon: None,
                    summary: None,
                    health: None,
                    artifacts: vec![artifact("linux-x86_64", Format::TarGz, "wazuh-warden.tar.gz")],
                },
            ],
        }
    }

    pub fn artifact(target: &str, format: Format, name: &str) -> Artifact {
        Artifact {
            target: target.into(),
            format,
            url: format!("https://example.test/dl/{name}"),
            sha256: "0".repeat(64),
            size: 1,
        }
    }

    fn reparse(m: &Manifest) -> Result<Manifest> {
        Manifest::parse(&serde_json::to_vec(m).unwrap())
    }

    #[test]
    fn round_trips_and_picks_the_preferred_format() {
        let m = reparse(&sample()).unwrap();
        let ram = m.warden("ram").unwrap();
        let deb = ram.artifact_for("linux-x86_64", &[Format::Deb, Format::TarGz]).unwrap();
        assert_eq!(deb.format, Format::Deb);
        let tgz = ram.artifact_for("linux-x86_64", &[Format::TarGz]).unwrap();
        assert_eq!(tgz.file_name().unwrap(), "ramwarden-0.3.0-linux-x86_64.tar.gz");
        assert!(ram.artifact_for("macos-aarch64", &[Format::TarGz]).is_none());
    }

    #[test]
    fn rejects_what_could_mislead_the_installer() {
        let mut m = sample();
        m.wardens[0].artifacts[0].url = "http://example.test/x.deb".into();
        assert!(reparse(&m).is_err(), "plain HTTP");

        let mut m = sample();
        m.wardens[0].artifacts[0].sha256 = "abc".into();
        assert!(reparse(&m).is_err(), "short hash");

        let mut m = sample();
        m.wardens[0].artifacts[0].url = "https://example.test/dl/..".into();
        assert!(reparse(&m).is_err(), "path climb");

        let mut m = sample();
        m.wardens[1].id = "ram".into();
        assert!(reparse(&m).is_err(), "duplicate id");

        let mut m = sample();
        m.wardens[0].health = Some("http://evil.test/health".into());
        assert!(reparse(&m).is_err(), "remote health");

        let mut m = sample();
        m.schema = 2;
        assert!(reparse(&m).is_err(), "future schema");
    }

    #[test]
    fn loopback_urls() {
        assert!(is_loopback_url("http://127.0.0.1:7823/health"));
        assert!(is_loopback_url("http://localhost/health"));
        assert!(is_loopback_url("http://[::1]:7823/"));
        assert!(!is_loopback_url("http://127.0.0.1.evil.test/"));
        assert!(!is_loopback_url("https://127.0.0.1/"));
    }
}
