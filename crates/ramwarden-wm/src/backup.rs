//! Config backups: copy before write, restore on demand.
//!
//! The same promise debloat makes: nothing is changed without a snapshot that
//! puts it back. Backups are plain copies named `<wm>-<unix seconds>.bak`, so
//! a user can restore one by hand without RamWarden.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ramwarden_platform::{Error, Result, WmKind};

/// Backups for every window manager, in one directory.
#[derive(Clone, Debug)]
pub struct ConfigBackups {
    dir: PathBuf,
}

impl ConfigBackups {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        ConfigBackups { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Copy `path` aside, then write `text` over it. Returns the backup.
    ///
    /// A config that does not exist yet is backed up as an empty file, so
    /// restoring it means "back to no settings".
    pub fn write(&self, kind: WmKind, path: &Path, text: &str) -> Result<PathBuf> {
        fs::create_dir_all(&self.dir).map_err(|e| Error::os(format!("{}: {e}", self.dir.display())))?;
        let old = fs::read_to_string(path).unwrap_or_default();
        let backup = self.next_name(kind);
        fs::write(&backup, old).map_err(|e| Error::os(format!("{}: {e}", backup.display())))?;
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, text).map_err(|e| Error::os(format!("{}: {e}", path.display())))?;
        Ok(backup)
    }

    /// Put a backup back over `path`.
    pub fn restore(&self, backup: &Path, path: &Path) -> Result<()> {
        let text = fs::read_to_string(backup).map_err(|e| Error::os(format!("{}: {e}", backup.display())))?;
        fs::write(path, text).map_err(|e| Error::os(format!("{}: {e}", path.display())))
    }

    /// The newest backup for this window manager.
    pub fn latest(&self, kind: WmKind) -> Option<PathBuf> {
        let prefix = format!("{}-", kind.as_str());
        fs::read_dir(&self.dir)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".bak"))
            })
            .max_by_key(|p| Self::stamp(p))
    }

    /// Seconds and a counter from the file name, for ordering.
    fn stamp(p: &Path) -> (u64, u32) {
        let name = p.file_stem().and_then(|n| n.to_str()).unwrap_or_default();
        let mut parts = name.rsplit('-');
        let n = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let secs = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        (secs, n)
    }

    fn next_name(&self, kind: WmKind) -> PathBuf {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        (0u32..)
            .map(|n| self.dir.join(format!("{}-{secs}-{n}.bak", kind.as_str())))
            .find(|p| !p.exists())
            .expect("an unused backup name")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_keeps_the_old_text_and_restore_puts_it_back() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("aerospace.toml");
        fs::write(&cfg, "gaps.inner.horizontal = 4\n").unwrap();
        let b = ConfigBackups::new(tmp.path().join("backups"));

        let first = b.write(WmKind::AeroSpace, &cfg, "gaps.inner.horizontal = 8\n").unwrap();
        let second = b.write(WmKind::AeroSpace, &cfg, "gaps.inner.horizontal = 12\n").unwrap();
        assert_ne!(first, second);
        assert_eq!(b.latest(WmKind::AeroSpace), Some(second.clone()));
        assert_eq!(b.latest(WmKind::Sway), None);

        b.restore(&first, &cfg).unwrap();
        assert_eq!(fs::read_to_string(&cfg).unwrap(), "gaps.inner.horizontal = 4\n");
    }

    #[test]
    fn a_missing_config_backs_up_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("sway").join("config");
        let b = ConfigBackups::new(tmp.path().join("backups"));
        let backup = b.write(WmKind::Sway, &cfg, "gaps inner 4\n").unwrap();
        assert_eq!(fs::read_to_string(backup).unwrap(), "");
        assert_eq!(fs::read_to_string(cfg).unwrap(), "gaps inner 4\n");
    }
}
