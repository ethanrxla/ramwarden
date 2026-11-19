//! The places that fill a disk, and what may be done about each.
//!
//! Every entry is one of three kinds, and the kind is the safety rule:
//!
//! | Kind | What it is | What RamWarden may do |
//! |---|---|---|
//! | [`Kind::Clear`] | Regenerable: caches, temp files, crash dumps. The app that wrote it rebuilds it. | Delete the contents, after the user approves the list. |
//! | [`Kind::Review`] | The user's own files: Downloads, phone backups. | Show the size. Never deleted or moved by RamWarden. |
//! | [`Kind::Guide`] | Needs the owning app or administrator rights: the page file, Docker, the Recycle Bin, package caches owned by root. | Show the size and the exact steps. The user runs them. |
//!
//! The catalogue is data for all three operating systems in one table, chosen
//! at run time by `Platform::name`, so it is tested on any of them and policy
//! code carries no `cfg`.
//!
//! Left out on purpose, whatever their size: Windows Update's store, Defender,
//! the search index, drivers and Appx packages (the debloat rules), browser
//! profiles (cookies and sign-ins live beside the cache), and anything under
//! `Documents`, `Desktop` or a cloud-sync folder.
//!
//! The list started from what filled the reference PC's C: drive to 0.2 GB
//! free: caches, temp files, old build output, a 48 GB page file, Docker and
//! games.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Clear,
    Review,
    Guide,
}

/// One known place.
#[derive(Clone, Copy, Debug)]
pub struct Item {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    /// `"macos"`, `"linux"`, `"windows"`.
    pub os: &'static [&'static str],
    /// Templates over [`Places`]: `{home}/.cache`. `/` separates parts on
    /// every OS, and a part of `*` matches every folder at that level.
    pub paths: &'static [&'static str],
    /// What it is and what clearing costs, in one sentence.
    pub note: &'static str,
    /// For [`Kind::Guide`] and [`Kind::Review`]: what the user does.
    pub steps: &'static [&'static str],
    /// Only entries untouched for `temp_min_age_hours` are cleared. For temp
    /// folders, where a running app may hold today's files.
    pub aged: bool,
}

const MAC: &[&str] = &["macos"];
const LINUX: &[&str] = &["linux"];
const WIN: &[&str] = &["windows"];
const UNIX: &[&str] = &["macos", "linux"];
const ALL: &[&str] = &["macos", "linux", "windows"];

const fn clear(id: &'static str, label: &'static str, os: &'static [&'static str], paths: &'static [&'static str], note: &'static str) -> Item {
    Item { id, label, kind: Kind::Clear, os, paths, note, steps: &[], aged: false }
}

const fn guide(
    id: &'static str,
    label: &'static str,
    os: &'static [&'static str],
    paths: &'static [&'static str],
    note: &'static str,
    steps: &'static [&'static str],
) -> Item {
    Item { id, label, kind: Kind::Guide, os, paths, note, steps, aged: false }
}

pub const CATALOG: &[Item] = &[
    // ── Regenerable ─────────────────────────────────────────────────────────
    clear("app-caches", "App caches", MAC, &["{home}/Library/Caches"], "Files apps downloaded or computed and can fetch again. Apps start a little slower once."),
    clear("app-caches", "App caches", LINUX, &["{home}/.cache"], "Files apps downloaded or computed and can fetch again. Apps start a little slower once."),
    Item {
        id: "temp-files",
        label: "Temporary files",
        kind: Kind::Clear,
        os: WIN,
        paths: &["{temp}"],
        note: "Leftovers from installers and apps. Only files untouched for a day are removed.",
        steps: &[],
        aged: true,
    },
    clear(
        "browser-caches",
        "Browser caches",
        WIN,
        &[
            "{local}/Google/Chrome/User Data/*/Cache",
            "{local}/Google/Chrome/User Data/*/Code Cache",
            "{local}/Microsoft/Edge/User Data/*/Cache",
            "{local}/Microsoft/Edge/User Data/*/Code Cache",
            "{local}/BraveSoftware/Brave-Browser/User Data/*/Cache",
            "{local}/BraveSoftware/Brave-Browser/User Data/*/Code Cache",
            "{local}/Mozilla/Firefox/Profiles/*/cache2",
        ],
        "Copies of pages and scripts. Sign-ins, history and open tabs are not in these folders and are kept.",
    ),
    clear("crash-dumps", "Crash dumps", WIN, &["{local}/CrashDumps", "{local}/D3DSCache"], "Memory dumps from crashed apps and compiled shader caches. Games rebuild shaders on next launch."),
    clear("xcode-derived", "Xcode build data", MAC, &["{home}/Library/Developer/Xcode/DerivedData", "{home}/Library/Developer/CoreSimulator/Caches"], "Xcode's build products and indexes. The next build of each project is a full one."),
    clear("logs", "App logs", MAC, &["{home}/Library/Logs"], "Diagnostic logs apps wrote. Nothing reads them unless you are reporting a bug."),
    clear("npm-cache", "npm download cache", UNIX, &["{home}/.npm/_cacache"], "Packages npm downloaded. It downloads them again when needed."),
    clear("npm-cache", "npm download cache", WIN, &["{local}/npm-cache/_cacache"], "Packages npm downloaded. It downloads them again when needed."),
    clear("pip-cache", "pip download cache", WIN, &["{local}/pip/Cache"], "Packages pip downloaded. It downloads them again when needed."),
    clear("cargo-cache", "Rust download cache", ALL, &["{home}/.cargo/registry/cache", "{home}/.cargo/registry/src"], "Crate archives and their unpacked sources. Cargo fetches them again on the next build."),
    clear("gradle-cache", "Gradle caches", ALL, &["{home}/.gradle/caches"], "Dependencies and build caches. Gradle downloads them again on the next build."),
    // ── The user's own files ────────────────────────────────────────────────
    Item {
        id: "downloads",
        label: "Downloads",
        kind: Kind::Review,
        os: ALL,
        paths: &["{home}/Downloads"],
        note: "Your files. RamWarden never deletes these.",
        steps: &["Open Downloads, sort by size, and delete installers and archives you have already used."],
        aged: false,
    },
    Item {
        id: "phone-backups",
        label: "iPhone and iPad backups",
        kind: Kind::Review,
        os: MAC,
        paths: &["{home}/Library/Application Support/MobileSync/Backup"],
        note: "Full device backups. Deleting one cannot be undone.",
        steps: &["Open Finder, select the device or Settings > General > Manage Backups, and delete backups of devices you no longer own."],
        aged: false,
    },
    // ── Needs the owning app or an administrator ────────────────────────────
    guide("trash", "Trash", MAC, &["{home}/.Trash"], "Files you already deleted, still on disk.", &["Right-click the Trash in the Dock and choose Empty Trash."]),
    guide("trash", "Trash", LINUX, &["{home}/.local/share/Trash"], "Files you already deleted, still on disk.", &["Open the file manager's Trash and choose Empty, or run: gio trash --empty"]),
    guide("trash", "Recycle Bin", WIN, &[], "Files you already deleted, still on disk. Its size needs Explorer to read.", &["Right-click the Recycle Bin on the desktop and choose Empty Recycle Bin."]),
    guide(
        "page-file",
        "Page file",
        WIN,
        &["{system}/pagefile.sys"],
        "Windows' swap space. It grows with commit charge and does not shrink by itself; 48 GB was seen on a 32 GB PC.",
        &[
            "Open System Properties > Advanced > Performance Settings > Advanced > Virtual memory > Change.",
            "Untick 'Automatically manage', pick the system drive, and set a custom size (initial 4096 MB, maximum 16384 MB for 32 GB of RAM), or move it to a drive with room.",
            "Click Set, then restart. Do not turn the page file off: apps that reserve more than they use will fail to start.",
        ],
    ),
    guide(
        "hibernation",
        "Hibernation file",
        WIN,
        &["{system}/hiberfil.sys"],
        "Holds RAM contents for hibernate and Fast Startup. About 40% of RAM.",
        &["If you never hibernate, open an administrator terminal and run: powercfg /hibernate off", "This also turns off Fast Startup. Undo with: powercfg /hibernate on"],
    ),
    guide(
        "windows-temp",
        "Windows temp and update leftovers",
        WIN,
        &["{system}/Windows/Temp"],
        "System-owned leftovers. Clearing needs administrator rights, so Windows' own tool does it.",
        &["Open Settings > System > Storage > Temporary files.", "Tick 'Windows Update Cleanup', 'Temporary files' and 'Delivery Optimization Files', then Remove files."],
    ),
    guide(
        "docker",
        "Docker images and volumes",
        WIN,
        &["{local}/Docker/wsl"],
        "Docker's virtual disk. It grows as images are pulled and does not shrink when they are removed.",
        &[
            "Remove what is unused: docker system prune -a (add --volumes only if no volume holds data you need).",
            "Then shrink the disk: quit Docker Desktop, run wsl --shutdown, and in Docker Desktop > Settings > Resources move the disk image to a drive with room.",
        ],
    ),
    guide(
        "docker",
        "Docker images and volumes",
        MAC,
        &["{home}/Library/Containers/com.docker.docker/Data"],
        "Docker's virtual disk. It grows as images are pulled.",
        &["Remove what is unused: docker system prune -a (add --volumes only if no volume holds data you need).", "Lower the limit in Docker Desktop > Settings > Resources > Virtual disk limit."],
    ),
    guide(
        "docker",
        "Docker images and volumes",
        LINUX,
        &["/var/lib/docker"],
        "Images, containers and volumes. Owned by root, so its size may not be readable.",
        &["See what is using it: docker system df", "Remove what is unused: docker system prune -a (add --volumes only if no volume holds data you need)."],
    ),
    guide(
        "package-cache",
        "Package manager cache",
        LINUX,
        &["/var/cache/apt/archives", "/var/cache/pacman/pkg", "/var/cache/dnf"],
        "Downloaded packages that are already installed.",
        &["Debian or Ubuntu: sudo apt clean", "Arch: sudo paccache -r", "Fedora: sudo dnf clean packages"],
    ),
    guide(
        "journal",
        "System logs",
        LINUX,
        &["/var/log/journal"],
        "The systemd journal. It keeps up to 10% of the disk unless capped.",
        &["Keep the last 200 MB: sudo journalctl --vacuum-size=200M"],
    ),
    guide(
        "homebrew",
        "Homebrew downloads",
        MAC,
        &["{home}/Library/Caches/Homebrew"],
        "Installers for formulae that are already installed. Counted inside App caches too.",
        &["Run: brew cleanup --prune=all"],
    ),
    guide(
        "games",
        "Steam games",
        WIN,
        &["{system}/Program Files (x86)/Steam/steamapps/common"],
        "Installed games. Steam can move one to another drive without downloading it again.",
        &["In Steam open Settings > Storage, add a library on a drive with room, then select games and choose Move."],
    ),
    guide(
        "games",
        "Steam games",
        LINUX,
        &["{home}/.local/share/Steam/steamapps/common"],
        "Installed games. Steam can move one to another drive without downloading it again.",
        &["In Steam open Settings > Storage, add a library on a drive with room, then select games and choose Move."],
    ),
];

/// The folders the templates stand on, read once from the environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Places {
    pub os: &'static str,
    pub home: PathBuf,
    /// The user's temp folder (`%TEMP%`).
    pub temp: Option<PathBuf>,
    /// `%LOCALAPPDATA%`.
    pub local: Option<PathBuf>,
    /// `C:` with no trailing separator.
    pub system: Option<PathBuf>,
    /// Folders no scan enters, because reading them makes the OS ask the
    /// user's permission. See [`MAC_PRIVATE`].
    pub private: Vec<PathBuf>,
}

/// macOS folders whose contents only an app the user has allowed may read
/// (TCC). Opening one makes macOS show "RamWarden would like to access files
/// in your Documents folder" or "...data from other apps", and a scan that
/// walks the home folder opened all of them: the first storage alert on the
/// reference Mac (2026-11-19) came with a permission prompt. An unsigned
/// build is a new app to TCC after every rebuild, so a grant did not last
/// either. The scan leaves them out and says so, unless the user has given
/// RamWarden Full Disk Access, which is one grant that never prompts.
/// Paths are relative to the home folder.
pub const MAC_PRIVATE: &[&str] = &[
    "Desktop",
    "Documents",
    "Downloads",
    "Library/Mobile Documents",
    "Library/CloudStorage",
    "Library/Containers",
    "Library/Group Containers",
    "Library/Mail",
    "Library/Messages",
    "Library/Safari",
    "Library/Calendars",
    "Library/Reminders",
    "Library/Cookies",
    "Library/HomeKit",
    "Library/Suggestions",
    "Library/Metadata/CoreSpotlight",
    "Library/Application Support/AddressBook",
    "Library/Application Support/CallHistoryDB",
    "Library/Application Support/com.apple.TCC",
    "Library/Application Support/Knowledge",
    "Pictures/Photos Library.photoslibrary",
    // Media & Apple Music: opening the Music or TV library makes macOS ask
    // to "access Apple Music, your music and video activity, and your media
    // library" (reference Mac, 2026-11-19).
    "Music/Music",
    "Music/iTunes",
    "Movies/TV",
];

/// Whether this process may read every folder without macOS asking. The TCC
/// database is readable only with Full Disk Access, and a refused open fails
/// quietly: Full Disk Access is never prompted for, unlike the folders above.
pub fn full_disk_access(home: &Path) -> bool {
    std::fs::File::open(home.join("Library/Application Support/com.apple.TCC/TCC.db")).is_ok()
}

impl Places {
    pub fn from_env(os: &'static str) -> Self {
        let var = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| p.is_absolute());
        Places {
            os,
            // A missing HOME falls back to `/`; nothing is ever cleared from there.
            home: Some(ramwarden_core::config::home()).filter(|h| h.parent().is_some()).unwrap_or_default(),
            temp: (os == "windows").then(|| var("TEMP")).flatten(),
            local: (os == "windows").then(|| var("LOCALAPPDATA")).flatten(),
            // `C:` alone is drive-relative; the separator makes it the root.
            system: (os == "windows").then(|| PathBuf::from(format!("{}\\", std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into())))),
            private: Vec::new(),
        }
        .guarded()
    }

    /// Fill in [`Places::private`] for this OS. Nothing on Linux or Windows.
    pub fn guarded(mut self) -> Self {
        self.private = if self.os == "macos" && self.home.parent().is_some() && !full_disk_access(&self.home) {
            MAC_PRIVATE.iter().map(|p| self.home.join(p)).collect()
        } else {
            Vec::new()
        };
        self
    }

    /// Whether every folder a template can name is private: the part before
    /// any `*` already is.
    pub fn template_private(&self, template: &str) -> bool {
        let Some(rest) = template.strip_prefix("{home}") else { return false };
        let fixed = rest.split('/').filter(|p| !p.is_empty()).take_while(|p| *p != "*").fold(self.home.clone(), |d, p| d.join(p));
        self.is_private(&fixed)
    }

    /// Whether `path` is, or is inside, a folder no scan enters.
    pub fn is_private(&self, path: &Path) -> bool {
        self.private.iter().any(|p| path.starts_with(p))
    }

    /// The catalogue for this OS.
    pub fn items(&self) -> impl Iterator<Item = &'static Item> + '_ {
        CATALOG.iter().filter(|i| i.os.contains(&self.os))
    }

    pub fn item(&self, id: &str) -> Option<&'static Item> {
        self.items().find(|i| i.id == id)
    }

    /// Every existing folder a template names. A `*` part lists that level.
    pub fn resolve(&self, template: &str) -> Vec<PathBuf> {
        let mut parts = template.split('/').filter(|p| !p.is_empty());
        let first = if template.starts_with('/') { Some(PathBuf::from("/")) } else {
            match parts.next() {
                Some("{home}") => Some(self.home.clone()),
                Some("{temp}") => self.temp.clone(),
                Some("{local}") => self.local.clone(),
                Some("{system}") => self.system.clone(),
                _ => None,
            }
        };
        let Some(first) = first.filter(|p| p.is_absolute()) else { return Vec::new() };
        let mut found = vec![first];
        for part in parts {
            // Not even listed or looked up: either can make macOS ask.
            found.retain(|p| !self.is_private(p));
            found = if part == "*" {
                found
                    .iter()
                    .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path())
                    .collect()
            } else {
                found.into_iter().map(|dir| dir.join(part)).collect()
            };
        }
        found.retain(|p| !self.is_private(p) && std::fs::symlink_metadata(p).is_ok());
        found
    }

    /// Whether RamWarden may delete the contents of `dir`. It must be a real
    /// folder (not a link) that resolves to somewhere below the home, temp or
    /// local-data folder, and may not be one of the user's own top-level
    /// folders. Checked again at the moment of deleting, on the path as it
    /// then resolves, so a folder swapped for a link in between is refused.
    pub fn may_clear(&self, dir: &Path) -> bool {
        if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
            return false;
        }
        let Ok(real) = dir.canonicalize() else { return false };
        // The path as written must sit under the base, and resolve to the
        // same place as the base's real path plus that remainder. A link at
        // any step below the base (`~/.cache/x -> ~/Documents`) changes the
        // resolved path and is refused, not just a link at the last step.
        let below = |base: &Path, min_depth: usize| -> Option<PathBuf> {
            let real_base = base.canonicalize().ok()?;
            let rest = dir.strip_prefix(base).or_else(|_| dir.strip_prefix(&real_base)).ok()?;
            if rest.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
                return None;
            }
            (real_base.join(rest) == real && rest.components().count() >= min_depth).then(|| rest.to_path_buf())
        };
        if self.temp.as_deref().is_some_and(|t| below(t, 0).is_some()) || self.local.as_deref().is_some_and(|l| below(l, 1).is_some()) {
            return true;
        }
        if self.home.as_os_str().is_empty() {
            return false;
        }
        below(&self.home, 1).is_some_and(|rest| {
            let top = rest.components().next().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).unwrap_or_default();
            rest.components().count() > 1 || !PERSONAL.iter().any(|p| top == *p || top.starts_with("onedrive"))
        })
    }
}

/// Top-level home folders that are the user's own. Their contents are never
/// cleared wholesale; a tagged cache deeper inside one still may be.
const PERSONAL: &[&str] = &[
    "documents", "desktop", "downloads", "pictures", "music", "movies", "videos", "public", "dropbox", "google drive", "icloud drive",
    "library", "appdata", "applications",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique_per_os_and_only_clear_items_lack_steps() {
        for os in ALL {
            let mut seen = HashSet::new();
            for item in CATALOG.iter().filter(|i| i.os.contains(os)) {
                assert!(seen.insert(item.id), "{} twice on {os}", item.id);
                assert_eq!(item.kind == Kind::Clear, item.steps.is_empty(), "{}", item.id);
                assert!(!item.note.is_empty());
            }
        }
    }

    #[test]
    fn nothing_clearable_is_outside_the_user_folders() {
        for item in CATALOG.iter().filter(|i| i.kind == Kind::Clear) {
            for p in item.paths {
                assert!(p.starts_with("{home}/") || p.starts_with("{local}/") || *p == "{temp}", "{}: {p}", item.id);
                for never in ["Documents", "Desktop", "Windows", "Defender", "SoftwareDistribution", "Packages"] {
                    assert!(!p.contains(never), "{}: {p}", item.id);
                }
            }
        }
    }

    #[test]
    fn templates_resolve_and_stars_expand() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("Local");
        for profile in ["Default", "Profile 1"] {
            std::fs::create_dir_all(local.join("Google/Chrome/User Data").join(profile).join("Cache")).unwrap();
        }
        std::fs::create_dir_all(local.join("Google/Chrome/User Data/Default/Network")).unwrap();
        let places = Places { os: "windows", home: dir.path().join("home"), temp: None, local: Some(local.clone()), system: None, private: Vec::new() };
        let mut found = places.resolve("{local}/Google/Chrome/User Data/*/Cache");
        found.sort();
        assert_eq!(found.len(), 2);
        assert!(found[0].ends_with("Default/Cache") && found[1].ends_with("Profile 1/Cache"));
        // Unknown bases and missing folders resolve to nothing.
        assert!(places.resolve("{temp}").is_empty());
        assert!(places.resolve("{home}/.cache").is_empty());
        assert!(places.resolve("relative/path").is_empty());
    }

    #[test]
    fn clearing_is_confined_to_deep_user_folders() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".cache/app")).unwrap();
        std::fs::create_dir_all(home.join("Documents")).unwrap();
        let outside = dir.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        let places = Places { os: "linux", home: home.clone(), ..Default::default() };
        assert!(places.may_clear(&home.join(".cache/app")));
        assert!(!places.may_clear(&home));
        assert!(places.may_clear(&home.join(".cache")));
        assert!(!places.may_clear(&home.join("Documents")), "the user's own folders are never cleared wholesale");
        assert!(!places.may_clear(&outside));
        assert!(!places.may_clear(&home.join("missing")));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, home.join(".cache/link")).unwrap();
            assert!(!places.may_clear(&home.join(".cache/link")), "a link out of home is refused");
            // A link part-way down: `.cache/docs` points at Documents, so
            // `.cache/docs/Work` resolves inside the user's own folder.
            std::fs::create_dir_all(home.join("Documents/Work")).unwrap();
            std::os::unix::fs::symlink(home.join("Documents"), home.join(".cache/docs")).unwrap();
            assert!(!places.may_clear(&home.join(".cache/docs/Work")), "a link below the base is refused");
            assert!(!places.may_clear(&home.join(".cache/app/../../Documents")), "dot-dot is refused");
        }
    }
}
