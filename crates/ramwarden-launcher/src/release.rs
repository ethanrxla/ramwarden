//! The publishing side: build the next manifest from a directory of installers.
//!
//! Installers are named `<anything>-<os>-<arch>.<format>`, for example
//! `ramwarden-windows-x86_64.exe` or `ramwarden-linux-x86_64.deb`;
//! the name alone says which machine each is for. Files that do not follow the
//! pattern (the macOS `.pkg`, which is for first installs only, checksums,
//! the manifest itself) are left out.
//!
//! A release replaces one warden's entry and carries every other warden over
//! from the previous manifest, so the RAM warden's release does not drop the
//! Wazuh warden and vice versa. `sequence` is the previous one plus one.

use std::path::Path;

use crate::manifest::{Artifact, Format, Manifest, SCHEMA, Tier, WardenRelease};
use crate::updater::sha256_file;
use crate::{Error, Result};

/// The warden being released, as the command line describes it.
pub struct Spec {
    pub id: String,
    pub name: String,
    pub tier: Tier,
    pub version: String,
    pub icon: Option<String>,
    pub summary: Option<String>,
    pub health: Option<String>,
    /// Asset URLs are `<base_url>/<file name>`.
    pub base_url: String,
}

/// `(target, format)` from an asset's file name, if it is an update asset.
pub fn classify(name: &str) -> Option<(String, Format)> {
    let (stem, format) = if let Some(s) = name.strip_suffix(".tar.gz") {
        (s, Format::TarGz)
    } else if let Some(s) = name.strip_suffix(".deb") {
        (s, Format::Deb)
    } else {
        (name.strip_suffix(".exe")?, Format::Exe)
    };
    let mut parts = stem.rsplitn(3, '-');
    let arch = parts.next()?;
    let os = parts.next()?;
    parts.next()?;
    let ok = matches!(
        (os, format),
        ("windows", Format::Exe) | ("linux", Format::Deb | Format::TarGz) | ("macos", Format::TarGz)
    ) && matches!(arch, "x86_64" | "aarch64");
    ok.then(|| (format!("{os}-{arch}"), format))
}

impl Spec {
    /// The next manifest: `previous` with this warden's entry replaced.
    pub fn manifest(&self, dist: &Path, previous: Option<Manifest>, published_at: String) -> Result<Manifest> {
        let mut artifacts = Vec::new();
        let mut names: Vec<_> = std::fs::read_dir(dist)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        for name in names {
            if let Some((target, format)) = classify(&name) {
                let (sha256, size) = sha256_file(&dist.join(&name))?;
                artifacts.push(Artifact {
                    target,
                    format,
                    url: format!("{}/{name}", self.base_url.trim_end_matches('/')),
                    sha256,
                    size,
                });
            }
        }
        if artifacts.is_empty() {
            return Err(Error::Manifest(format!(
                "no <name>-<os>-<arch>.<exe|deb|tar.gz> files in {}",
                dist.display()
            )));
        }
        let entry = WardenRelease {
            id: self.id.clone(),
            name: self.name.clone(),
            tier: self.tier,
            version: self.version.clone(),
            icon: self.icon.clone(),
            summary: self.summary.clone(),
            health: self.health.clone(),
            artifacts,
        };
        let (sequence, mut wardens) = match previous {
            Some(p) => (p.sequence + 1, p.wardens),
            None => (1, Vec::new()),
        };
        match wardens.iter_mut().find(|w| w.id == entry.id) {
            Some(w) => *w = entry,
            None => wardens.push(entry),
        }
        let m = Manifest {
            schema: SCHEMA,
            sequence,
            published_at,
            wardens,
        };
        // Same checks the launcher will run, so a bad release fails here.
        Manifest::parse(&serde_json::to_vec(&m).map_err(|e| Error::Manifest(e.to_string()))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_asset_names() {
        assert_eq!(
            classify("ramwarden-0.3.0-windows-x86_64.exe"),
            Some(("windows-x86_64".into(), Format::Exe))
        );
        assert_eq!(
            classify("ramwarden-0.3.0-linux-x86_64.deb"),
            Some(("linux-x86_64".into(), Format::Deb))
        );
        assert_eq!(
            classify("ramwarden-0.3.0-macos-aarch64.tar.gz"),
            Some(("macos-aarch64".into(), Format::TarGz))
        );
        assert_eq!(
            classify("ramwarden-windows-x86_64.exe"),
            Some(("windows-x86_64".into(), Format::Exe))
        );
        assert_eq!(classify("RamWarden.pkg"), None);
        assert_eq!(classify("ramwarden-0.3.0-macos-aarch64.deb"), None);
        assert_eq!(classify("manifest.json"), None);
    }

    fn spec(id: &str, version: &str) -> Spec {
        Spec {
            id: id.into(),
            name: id.into(),
            tier: Tier::Free,
            version: version.into(),
            icon: None,
            summary: None,
            health: None,
            base_url: "https://example.test/v1/".into(),
        }
    }

    #[test]
    fn builds_and_carries_other_wardens_forward() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("ramwarden-0.3.0-linux-x86_64.tar.gz"), b"abc").unwrap();
        std::fs::write(d.path().join("RamWarden-0.3.0.pkg"), b"pkg").unwrap();
        let first = spec("ram", "0.3.0").manifest(d.path(), None, "t".into()).unwrap();
        assert_eq!(first.sequence, 1);
        let a = &first.wardens[0].artifacts;
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].url, "https://example.test/v1/ramwarden-0.3.0-linux-x86_64.tar.gz");
        assert_eq!(a[0].size, 3);
        assert_eq!(a[0].sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");

        let next = spec("wazuh", "0.1.0").manifest(d.path(), Some(first), "t".into()).unwrap();
        assert_eq!(next.sequence, 2);
        assert_eq!(next.wardens.len(), 2);
        let again = spec("ram", "0.3.1").manifest(d.path(), Some(next), "t".into()).unwrap();
        assert_eq!(again.sequence, 3);
        assert_eq!(again.warden("ram").unwrap().version, "0.3.1");
        assert!(again.warden("wazuh").is_some());
    }

    #[test]
    fn an_empty_dist_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(spec("ram", "0.3.0").manifest(d.path(), None, "t".into()).is_err());
    }
}
