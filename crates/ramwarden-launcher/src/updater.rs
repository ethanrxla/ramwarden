//! Check, download, verify, install: the update loop for every warden.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fetch::{Fetch, MANIFEST_LIMIT};
use crate::install::{Installer, Ran};
use crate::manifest::{Format, Manifest, Tier, WardenRelease};
use crate::signing::Verifier;
use crate::state::State;
use crate::target::Target;
use crate::{Error, Result};

/// The RAM warden ships in the same installer as the launcher.
pub const RAM: &str = "ram";

/// One warden's standing after a check.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Available {
    pub id: String,
    pub name: String,
    pub tier: Tier,
    /// `None`: not installed on this machine.
    pub installed: Option<String>,
    pub latest: String,
    /// A newer version than the installed one exists (always false when not
    /// installed: installing is a choice, updating is upkeep).
    pub update: bool,
    /// Paid, and this device has no pass to fetch it with.
    pub locked: bool,
    /// The manifest has a build for this machine.
    pub supported: bool,
}

/// What an apply did. A started installer is reported as started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    Installed { id: String, version: String },
    Started { id: String, version: String },
    UpToDate { id: String, version: String },
}

pub struct Updater<F: Fetch> {
    pub fetch: F,
    pub verifier: Verifier,
    pub target: Target,
    pub manifest_url: String,
    /// State and `staging/` live here.
    pub dir: PathBuf,
    pub device_token: Option<String>,
    /// The running launcher's version, which is the RAM warden's.
    pub own_version: semver::Version,
    pub is_root: bool,
}

impl<F: Fetch> Updater<F> {
    /// Fetch, verify and replay-check the manifest; record what it offers.
    pub fn check(&self, state: &mut State) -> Result<Manifest> {
        let result = self.fetch_manifest(state);
        state.last_check = Some(now());
        match &result {
            Ok(m) => {
                state.highest_sequence = state.highest_sequence.max(m.sequence);
                self.confirm_pending(state);
                state.available = self.available(m, state);
                state.last_error = None;
            }
            Err(e) => state.last_error = Some(e.to_string()),
        }
        result
    }

    fn fetch_manifest(&self, state: &State) -> Result<Manifest> {
        if self.verifier.is_empty() {
            return Err(Error::NoKeys);
        }
        let bytes = self.fetch.get(&self.manifest_url, MANIFEST_LIMIT)?;
        let sig = self.fetch.get(&format!("{}.sig", self.manifest_url), 4096)?;
        self.verifier
            .verify(&bytes, &String::from_utf8_lossy(&sig))?;
        let m = Manifest::parse(&bytes)?;
        if m.sequence < state.highest_sequence {
            return Err(Error::Replayed {
                got: m.sequence,
                seen: state.highest_sequence,
            });
        }
        Ok(m)
    }

    /// The version installed for `id`, if any.
    pub fn installed(&self, id: &str, state: &State) -> Option<semver::Version> {
        if id == RAM {
            return Some(self.own_version.clone());
        }
        state
            .installed
            .get(id)
            .and_then(|v| semver::Version::parse(v).ok())
    }

    /// The Windows RAM installer finishes after the launcher exits; the next
    /// launcher to run is the new one, and its own version is the proof.
    fn confirm_pending(&self, state: &mut State) {
        let own = self.own_version.to_string();
        if state.pending.get(RAM) == Some(&own) {
            state.pending.remove(RAM);
        }
    }

    pub fn available(&self, m: &Manifest, state: &State) -> Vec<Available> {
        m.wardens
            .iter()
            .map(|w| {
                let installed = self.installed(&w.id, state);
                Available {
                    id: w.id.clone(),
                    name: w.name.clone(),
                    tier: w.tier,
                    update: installed.as_ref().is_some_and(|v| w.semver() > *v),
                    installed: installed.map(|v| v.to_string()),
                    latest: w.version.clone(),
                    locked: w.tier == Tier::Pro && self.device_token.is_none(),
                    supported: w
                        .artifact_for(&self.target.triple(), &self.target.formats)
                        .is_some(),
                }
            })
            .collect()
    }

    /// Install `id` at the manifest's version, or report it is current.
    /// Never installs an older version than the one present.
    pub fn apply(&self, m: &Manifest, id: &str, state: &mut State) -> Result<Applied> {
        let w = m
            .warden(id)
            .ok_or_else(|| Error::Unsupported(format!("no warden called {id} in this release")))?;
        if let Some(v) = self.installed(id, state)
            && w.semver() <= v
        {
            return Ok(Applied::UpToDate {
                id: id.into(),
                version: v.to_string(),
            });
        }
        let token = match w.tier {
            Tier::Free => None,
            Tier::Pro => Some(
                self.device_token
                    .as_deref()
                    .ok_or_else(|| Error::NeedsPro(w.name.clone()))?,
            ),
        };
        let file = self.download(w, token)?;
        let artifact_format = self.format_for(w)?;
        let unpack_dir = self.dir.join("staging").join(format!("{}-{}", w.id, w.version));
        let ran = Installer {
            format: artifact_format,
            file: &file,
            unpack_dir: &unpack_dir,
            replaces_self: id == RAM,
            is_root: self.is_root,
        }
        .run();
        // The download is not kept: a rollback is a reinstall of the older
        // release's installer, which the releases page keeps.
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir_all(&unpack_dir);
        let (id, version) = (w.id.clone(), w.version.clone());
        match ran? {
            Ran::Finished => {
                state.pending.remove(&id);
                if id != RAM {
                    state.installed.insert(id.clone(), version.clone());
                }
                Ok(Applied::Installed { id, version })
            }
            Ran::Started => {
                state.pending.insert(id.clone(), version.clone());
                Ok(Applied::Started { id, version })
            }
        }
    }

    fn format_for(&self, w: &WardenRelease) -> Result<Format> {
        w.artifact_for(&self.target.triple(), &self.target.formats)
            .map(|a| a.format)
            .ok_or_else(|| Error::NoArtifact {
                warden: w.name.clone(),
                target: self.target.triple(),
            })
    }

    /// Download into `staging/`, checking size and SHA-256 against the signed
    /// manifest. A mismatch deletes the file: nothing unverified is left where
    /// a later step could run it.
    pub fn download(&self, w: &WardenRelease, bearer: Option<&str>) -> Result<PathBuf> {
        let a = w
            .artifact_for(&self.target.triple(), &self.target.formats)
            .ok_or_else(|| Error::NoArtifact {
                warden: w.name.clone(),
                target: self.target.triple(),
            })?;
        let staging = self.dir.join("staging");
        std::fs::create_dir_all(&staging)?;
        let name = a.file_name()?;
        let part = staging.join(format!("{name}.part"));
        let mut sink = Hashing::new(std::fs::File::create(&part)?);
        let got = self.fetch.fetch(&a.url, bearer, a.size, &mut sink);
        let (file, digest) = sink.finish();
        drop(file);
        let checked = got.and_then(|n| {
            let actual = hex::encode(digest);
            if n != a.size || !actual.eq_ignore_ascii_case(&a.sha256) {
                Err(Error::Mismatch {
                    file: name.clone(),
                    expected: format!("{} bytes, sha256 {}", a.size, a.sha256),
                    actual: format!("{n} bytes, sha256 {actual}"),
                })
            } else {
                Ok(())
            }
        });
        if let Err(e) = checked {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        let done = staging.join(&name);
        std::fs::rename(&part, &done)?;
        Ok(done)
    }

    /// Health of every warden the manifest lists with a health URL.
    pub fn running(&self, m: &Manifest) -> Vec<(String, bool)> {
        m.wardens
            .iter()
            .filter_map(|w| w.health.as_ref().map(|h| (w.id.clone(), self.fetch.healthy(h))))
            .collect()
    }
}

/// Order for `update all`: the RAM warden first, since it carries the
/// launcher and a newer launcher may be needed to install the rest.
pub fn update_order(available: &[Available]) -> Vec<String> {
    let mut ids: Vec<String> = available
        .iter()
        .filter(|a| a.update && !a.locked && a.supported)
        .map(|a| a.id.clone())
        .collect();
    ids.sort_by_key(|id| id != RAM);
    ids
}

struct Hashing<W> {
    inner: W,
    hash: Sha256,
}

impl<W: Write> Hashing<W> {
    fn new(inner: W) -> Self {
        Hashing {
            inner,
            hash: Sha256::new(),
        }
    }
    fn finish(self) -> (W, [u8; 32]) {
        (self.inner, self.hash.finalize().into())
    }
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// SHA-256 of a file, hex. Used by `release manifest`.
pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Hashing::new(std::io::sink());
    let n = std::io::copy(&mut f, &mut h)?;
    Ok((hex::encode(h.finish().1), n))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;
    use crate::manifest::tests::sample;
    use crate::signing::Signer;

    /// Serves fixed bodies by URL and records bearer tokens.
    #[derive(Default)]
    struct Fake {
        bodies: HashMap<String, Vec<u8>>,
        bearers: RefCell<Vec<Option<String>>>,
    }

    impl Fetch for Fake {
        fn fetch(&self, url: &str, bearer: Option<&str>, limit: u64, sink: &mut dyn Write) -> Result<u64> {
            self.bearers.borrow_mut().push(bearer.map(str::to_string));
            let body = self
                .bodies
                .get(url)
                .ok_or_else(|| Error::Fetch(format!("404 {url}")))?;
            if body.len() as u64 > limit {
                return Err(Error::Fetch("too big".into()));
            }
            sink.write_all(body)?;
            Ok(body.len() as u64)
        }
        fn healthy(&self, url: &str) -> bool {
            url.contains("7823")
        }
        fn post_json(&self, url: &str, _: &serde_json::Value) -> Result<(u16, Vec<u8>)> {
            Err(Error::Fetch(format!("no POST in these tests: {url}")))
        }
    }

    const URL: &str = "https://example.test/manifest.json";

    struct Rig {
        up: Updater<Fake>,
        signer: Signer,
        _dir: tempfile::TempDir,
    }

    fn rig(own: &str, token: Option<&str>) -> Rig {
        let signer = Signer::generate().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let up = Updater {
            fetch: Fake::default(),
            verifier: Verifier::from_lines(&signer.public_b64()).unwrap(),
            target: Target {
                os: "linux",
                arch: "x86_64",
                formats: vec![Format::TarGz],
            },
            manifest_url: URL.into(),
            dir: dir.path().to_path_buf(),
            device_token: token.map(str::to_string),
            own_version: semver::Version::parse(own).unwrap(),
            is_root: false,
        };
        Rig {
            up,
            signer,
            _dir: dir,
        }
    }

    impl Rig {
        fn publish(&mut self, m: &Manifest) {
            let bytes = serde_json::to_vec(m).unwrap();
            let sig = self.signer.sign(&bytes);
            self.up.fetch.bodies.insert(URL.into(), bytes);
            self.up.fetch.bodies.insert(format!("{URL}.sig"), sig.into_bytes());
        }
    }

    #[test]
    fn check_reports_updates_locks_and_support() {
        let mut r = rig("0.2.1", None);
        r.publish(&sample());
        let mut st = State::default();
        r.up.check(&mut st).unwrap();
        assert_eq!(st.highest_sequence, 3);
        let ram = st.available.iter().find(|a| a.id == "ram").unwrap();
        assert!(ram.update && !ram.locked && ram.supported);
        assert_eq!(ram.installed.as_deref(), Some("0.2.1"));
        let wz = st.available.iter().find(|a| a.id == "wazuh").unwrap();
        assert!(wz.locked && !wz.update && wz.installed.is_none());
        assert_eq!(update_order(&st.available), ["ram"]);
    }

    #[test]
    fn unsigned_or_tampered_manifests_are_refused() {
        let mut r = rig("0.2.1", None);
        r.publish(&sample());
        let mut bytes = r.up.fetch.bodies[URL].clone();
        bytes[10] ^= 1;
        r.up.fetch.bodies.insert(URL.into(), bytes);
        let mut st = State::default();
        assert!(matches!(r.up.check(&mut st), Err(Error::BadSignature)));
        assert!(st.last_error.is_some());
        assert!(st.available.is_empty());
    }

    #[test]
    fn a_replayed_older_manifest_is_refused() {
        let mut r = rig("0.2.1", None);
        r.publish(&sample());
        let mut st = State {
            highest_sequence: 4,
            ..State::default()
        };
        assert!(matches!(r.up.check(&mut st), Err(Error::Replayed { got: 3, seen: 4 })));
        assert_eq!(st.highest_sequence, 4);
    }

    #[test]
    fn a_build_without_keys_never_fetches() {
        let mut r = rig("0.2.1", None);
        r.up.verifier = Verifier::from_lines("").unwrap();
        assert!(matches!(r.up.check(&mut State::default()), Err(Error::NoKeys)));
        assert!(r.up.fetch.bearers.borrow().is_empty());
    }

    fn with_payload(r: &mut Rig, id: &str, payload: &[u8], claimed: &[u8]) -> Manifest {
        let mut m = sample();
        let w = m.wardens.iter_mut().find(|w| w.id == id).unwrap();
        let a = w.artifacts.iter_mut().find(|a| a.format == Format::TarGz).unwrap();
        a.sha256 = hex::encode(Sha256::digest(claimed));
        a.size = claimed.len() as u64;
        r.up.fetch.bodies.insert(a.url.clone(), payload.to_vec());
        r.publish(&m);
        m
    }

    #[test]
    fn a_download_that_does_not_match_the_manifest_is_deleted() {
        let mut r = rig("0.2.1", None);
        let m = with_payload(&mut r, "ram", b"evil", b"good");
        let err = r.up.download(m.warden("ram").unwrap(), None).unwrap_err();
        assert!(matches!(err, Error::Mismatch { .. }), "{err}");
        let left: Vec<_> = std::fs::read_dir(r.up.dir.join("staging")).unwrap().collect();
        assert!(left.is_empty(), "nothing unverified remains");
    }

    #[test]
    fn pro_wardens_need_a_token_and_send_it() {
        let mut r = rig("0.2.1", None);
        let m = with_payload(&mut r, "wazuh", b"x", b"x");
        let mut st = State::default();
        assert!(matches!(r.up.apply(&m, "wazuh", &mut st), Err(Error::NeedsPro(_))));

        r.up.device_token = Some("tok".into());
        let path = r.up.download(m.warden("wazuh").unwrap(), Some("tok")).unwrap();
        assert!(path.is_file());
        assert_eq!(r.up.fetch.bearers.borrow().last().unwrap().as_deref(), Some("tok"));
    }

    #[test]
    fn never_downgrades() {
        let mut r = rig("0.9.0", None);
        let m = with_payload(&mut r, "ram", b"x", b"x");
        let got = r.up.apply(&m, "ram", &mut State::default()).unwrap();
        assert_eq!(got, Applied::UpToDate { id: "ram".into(), version: "0.9.0".into() });
    }

    #[cfg(unix)]
    #[test]
    fn installs_a_tarball_addon_and_records_it() {
        let mut r = rig("0.2.1", Some("tok"));
        let t = tempfile::tempdir().unwrap();
        let tgz = t.path().join("w.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(std::fs::File::create(&tgz).unwrap(), flate2::Compression::fast());
            let mut b = tar::Builder::new(gz);
            let script = b"exit 0\n";
            let mut h = tar::Header::new_gnu();
            h.set_size(script.len() as u64);
            h.set_mode(0o755);
            b.append_data(&mut h, "install.sh", &script[..]).unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        let bytes = std::fs::read(&tgz).unwrap();
        let m = with_payload(&mut r, "wazuh", &bytes, &bytes);
        let mut st = State::default();
        let got = r.up.apply(&m, "wazuh", &mut st).unwrap();
        assert_eq!(got, Applied::Installed { id: "wazuh".into(), version: "0.1.0".into() });
        assert_eq!(st.installed.get("wazuh").map(String::as_str), Some("0.1.0"));
        let again = r.up.apply(&m, "wazuh", &mut st).unwrap();
        assert!(matches!(again, Applied::UpToDate { .. }));
    }

    #[test]
    fn pending_windows_update_is_confirmed_by_the_new_launcher() {
        let mut r = rig("0.3.0", None);
        r.publish(&sample());
        let mut st = State::default();
        st.pending.insert("ram".into(), "0.3.0".into());
        r.up.check(&mut st).unwrap();
        assert!(st.pending.is_empty());
    }

    #[test]
    fn health_is_only_checked_where_listed() {
        let r = rig("0.2.1", None);
        assert_eq!(r.up.running(&sample()), [("ram".to_string(), true)]);
    }
}
