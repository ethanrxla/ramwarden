//! Private, incognito, and Tor browsing contexts.
//!
//! These matter because they are invisible to the browser extension: a private
//! window's tabs are not reported, so RamWarden sees a multi-hundred-megabyte
//! process with no explanation for it. Detecting the context at least lets the
//! UI say *what* the memory is, even when it cannot say which pages.
//!
//! # What is detectable, and what is not
//!
//! | Context | How | Closeable |
//! |---|---|---|
//! | Firefox private window | `wmctrl` window title | yes |
//! | Brave built-in Tor | `tor.mojom.TorLauncher` in the cmdline | no |
//! | Standalone Tor Browser | `torbrowser` in the exe path | no |
//! | **Brave incognito** | **not detectable** | no |
//!
//! Brave incognito is genuinely undetectable from outside: its renderers carry
//! no distinguishing flag, and the Flatpak sandbox hides its windows from
//! `wmctrl`. The only way to see those tabs is for the user to enable "Allow in
//! Private Windows" for the extension in `brave://extensions`. Reporting that
//! honestly is better than guessing.

use std::collections::HashMap;

use ramwarden_kernel::{Root, process, smaps};

use crate::exec;

/// What kind of private context this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    FirefoxPrivate,
    BraveTor,
    TorBrowser,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::FirefoxPrivate => "firefox_private",
            Kind::BraveTor => "brave_tor",
            Kind::TorBrowser => "tor_browser",
        }
    }

    /// A description for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Kind::FirefoxPrivate => "Firefox private window",
            Kind::BraveTor => "Brave (private window with Tor)",
            Kind::TorBrowser => "Tor Browser",
        }
    }
}

/// One private browsing context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateContext {
    pub kind: Kind,
    /// `wmctrl` window id, or empty when the window is not reachable.
    pub xid: String,
    pub pid: i32,
    pub title: String,
    /// Memory held by this context and its children, as PSS.
    pub pss: u64,
    /// Whether the extension can enumerate this context's tabs.
    pub can_get_urls: bool,
    /// Whether RamWarden can close it. False for anything `wmctrl` cannot see,
    /// which on this machine means everything Flatpak.
    pub closeable: bool,
}

/// Window-title markers for a Firefox private window, in the locales v1 handled.
const PRIVATE_TITLE_MARKERS: &[&str] = &["private browsing", "navigation privée"];

/// The utility process Brave spawns when its built-in Tor mode is active.
const BRAVE_TOR_MARKER: &str = "tor.mojom.TorLauncher";

/// Find private contexts in a `wmctrl` window list.
pub fn parse_firefox_private(window_list: &str) -> Vec<(String, i32, String)> {
    let mut out = Vec::new();
    for line in window_list.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        let Ok(pid) = fields[2].parse::<i32>() else {
            continue;
        };
        if pid <= 1 {
            continue;
        }
        let title = fields[4..].join(" ");
        let lower = title.to_lowercase();
        if PRIVATE_TITLE_MARKERS.iter().any(|m| lower.contains(m)) {
            out.push((fields[0].to_string(), pid, title));
        }
    }
    out
}

/// Sum PSS across a process and all its descendants.
fn tree_pss(root: &Root, pid: i32, children_of: &HashMap<i32, Vec<i32>>) -> u64 {
    let mut total = 0u64;
    let mut queue = vec![pid];
    let mut seen = std::collections::HashSet::from([pid]);
    while let Some(cur) = queue.pop() {
        if let Ok(r) = smaps::rollup(root, cur) {
            total += r.pss;
        }
        for &c in children_of.get(&cur).map(Vec::as_slice).unwrap_or(&[]) {
            if seen.insert(c) {
                queue.push(c);
            }
        }
    }
    total
}

/// Every private context detectable at the OS level.
///
/// `extension_has_incognito` reflects whether the browser extension has been
/// granted private-window access; it only changes whether tabs can be
/// enumerated, never whether the context is detected.
pub fn detect(root: &Root, extension_has_incognito: bool) -> Vec<PrivateContext> {
    let mut out = Vec::new();

    let table = process::table(root).unwrap_or_default();
    let mut children_of: HashMap<i32, Vec<i32>> = HashMap::new();
    for p in &table {
        children_of.entry(p.ppid).or_default().push(p.pid);
    }

    // Firefox private windows — Firefox is not sandboxed here, so wmctrl sees it.
    let display = exec::display();
    if let Some(list) = exec::run_with_env("wmctrl", &["-l", "-p"], &[("DISPLAY", &display)]) {
        for (xid, pid, title) in parse_firefox_private(&list) {
            out.push(PrivateContext {
                kind: Kind::FirefoxPrivate,
                xid,
                pid,
                title,
                pss: tree_pss(root, pid, &children_of),
                can_get_urls: extension_has_incognito,
                closeable: true,
            });
        }
    }

    // Brave's built-in Tor mode, visible through the launcher's command line
    // even inside the Flatpak sandbox.
    for p in &table {
        if !p.comm.to_lowercase().contains("brave") {
            continue;
        }
        let Ok(cmd) = process::cmdline(root, p.pid) else {
            continue;
        };
        if !cmd.contains(BRAVE_TOR_MARKER) {
            continue;
        }
        out.push(PrivateContext {
            kind: Kind::BraveTor,
            xid: String::new(),
            pid: p.pid,
            title: Kind::BraveTor.label().to_string(),
            pss: tree_pss(root, p.pid, &children_of),
            // Tor tabs are unreachable even with the extension permission, and
            // Flatpak Brave is unreachable via wmctrl.
            can_get_urls: false,
            closeable: false,
        });
        break;
    }

    // A standalone Tor Browser, identified by where it is installed.
    for p in &table {
        let lower = p.comm.to_lowercase();
        if lower != "firefox" && lower != "firefox-bin" {
            continue;
        }
        let Ok(exe) = process::exe(root, p.pid) else {
            continue;
        };
        let exe = exe.to_string_lossy().to_lowercase();
        if !exe.contains("torbrowser") && !exe.contains("tor-browser") {
            continue;
        }
        out.push(PrivateContext {
            kind: Kind::TorBrowser,
            xid: String::new(),
            pid: p.pid,
            title: Kind::TorBrowser.label().to_string(),
            pss: tree_pss(root, p.pid, &children_of),
            can_get_urls: false,
            closeable: false,
        });
        break;
    }

    if !out.is_empty() {
        tracing::info!(
            "private contexts: {}",
            out.iter()
                .map(|c| format!("{}({} MB)", c.kind.as_str(), c.pss / 1_000_000))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    out
}

/// Close a private window by sending `WM_DELETE_WINDOW`.
///
/// Only works for windows `wmctrl` can see, which excludes everything Flatpak.
pub fn close(xid: &str) -> bool {
    if xid.is_empty() {
        return false;
    }
    let display = exec::display();
    exec::run_with_env("wmctrl", &["-i", "-c", xid], &[("DISPLAY", &display)]).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW_LIST: &str = "\
0x01000003  0 1234   pop-os  GitHub — Mozilla Firefox
0x01200005  1 5678   pop-os  Search — Mozilla Firefox Private Browsing
0x01400007  1 5678   pop-os  Recherche — Navigation privée
0x01600009  0 0      pop-os  no pid Private Browsing
0x01800011  0 9012   pop-os  Brave Browser
";

    #[test]
    fn finds_firefox_private_windows_by_title() {
        let found = parse_firefox_private(WINDOW_LIST);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].0, "0x01200005");
        assert_eq!(found[0].1, 5678);
    }

    /// v1 handled the French locale explicitly; keep it working.
    #[test]
    fn the_french_locale_marker_is_recognised() {
        let found = parse_firefox_private(WINDOW_LIST);
        assert!(found.iter().any(|(_, _, t)| t.contains("Navigation privée")));
    }

    #[test]
    fn an_ordinary_window_is_not_a_private_context() {
        let found = parse_firefox_private(WINDOW_LIST);
        assert!(!found.iter().any(|(x, _, _)| x == "0x01000003"));
        assert!(!found.iter().any(|(x, _, _)| x == "0x01800011"));
    }

    #[test]
    fn a_window_with_no_usable_pid_is_skipped() {
        let found = parse_firefox_private(WINDOW_LIST);
        assert!(!found.iter().any(|(_, pid, _)| *pid <= 1));
    }

    #[test]
    fn title_matching_is_case_insensitive() {
        let out = parse_firefox_private("0x1 0 500 host Foo PRIVATE BROWSING\n");
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn empty_or_failed_wmctrl_output_yields_nothing() {
        assert!(parse_firefox_private("").is_empty());
        assert!(parse_firefox_private("Cannot open display\n").is_empty());
    }

    /// Build a /proc where Brave is running with its Tor launcher.
    fn tor_fixture() -> (tempfile::TempDir, Root) {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        std::fs::create_dir_all(root.join("proc")).unwrap();
        std::fs::write(root.join("proc/stat"), "btime 1790097664\n").unwrap();

        let add = |pid: i32, comm: &str, ppid: i32, cmdline: &[u8], pss_kb: u64| {
            let d = root.join(format!("proc/{pid}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("stat"),
                format!(
                    "{pid} ({comm}) S {ppid} {pid} {pid} 0 -1 0 0 0 0 0 10 5 0 0 20 0 1 0 \
                     1000 1000 100 0 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0"
                ),
            )
            .unwrap();
            std::fs::write(d.join("cmdline"), cmdline).unwrap();
            std::fs::write(
                d.join("smaps_rollup"),
                format!("0-1 ---p 0 00:00 0 [rollup]\nRss: {pss_kb} kB\nPss: {pss_kb} kB\n"),
            )
            .unwrap();
        };

        add(100, "brave", 1, b"/app/brave/brave\0", 50_000);
        add(
            101,
            "brave",
            100,
            b"/app/brave/brave\0--utility-sub-type=tor.mojom.TorLauncher\0",
            30_000,
        );
        add(102, "tor", 101, b"/app/brave/tor\0", 20_000);
        (dir, root)
    }

    #[test]
    fn detects_braves_built_in_tor_mode_inside_the_flatpak_sandbox() {
        let (_d, root) = tor_fixture();
        let found = detect(&root, false);
        let tor: Vec<&PrivateContext> = found.iter().filter(|c| c.kind == Kind::BraveTor).collect();
        assert_eq!(tor.len(), 1, "{found:?}");
        assert_eq!(tor[0].pid, 101);
    }

    /// Flatpak Brave is unreachable via wmctrl, so claiming it is closeable
    /// would offer the user a button that silently does nothing.
    #[test]
    fn brave_tor_is_reported_as_not_closeable() {
        let (_d, root) = tor_fixture();
        let found = detect(&root, true);
        let tor = found.iter().find(|c| c.kind == Kind::BraveTor).unwrap();
        assert!(!tor.closeable);
        assert!(
            !tor.can_get_urls,
            "the extension permission does not reach Tor tabs"
        );
        assert!(tor.xid.is_empty());
    }

    #[test]
    fn tor_memory_includes_the_launchers_children() {
        let (_d, root) = tor_fixture();
        let found = detect(&root, false);
        let tor = found.iter().find(|c| c.kind == Kind::BraveTor).unwrap();
        // the launcher (30 MB) plus the tor daemon beneath it (20 MB)
        assert_eq!(tor.pss, (30_000 + 20_000) * 1024);
    }

    #[test]
    fn a_standalone_tor_browser_is_found_by_its_install_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        std::fs::create_dir_all(root.join("proc")).unwrap();
        std::fs::write(root.join("proc/stat"), "btime 1790097664\n").unwrap();
        let d = root.join("proc/200");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("stat"),
            "200 (firefox) S 1 200 200 0 -1 0 0 0 0 0 10 5 0 0 20 0 1 0 1000 1000 100 \
             0 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0",
        )
        .unwrap();
        std::fs::write(d.join("cmdline"), b"firefox\0".as_slice()).unwrap();
        std::fs::write(
            d.join("smaps_rollup"),
            "0-1 ---p 0 00:00 0 [rollup]\nRss: 400000 kB\nPss: 400000 kB\n",
        )
        .unwrap();
        std::os::unix::fs::symlink("/opt/tor-browser/firefox", d.join("exe")).unwrap();

        let found = detect(&root, false);
        let tb = found.iter().find(|c| c.kind == Kind::TorBrowser).unwrap();
        assert_eq!(tb.pid, 200);
        assert!(!tb.closeable);
    }

    #[test]
    fn an_ordinary_firefox_is_not_mistaken_for_tor_browser() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        std::fs::create_dir_all(root.join("proc")).unwrap();
        std::fs::write(root.join("proc/stat"), "btime 1790097664\n").unwrap();
        let d = root.join("proc/300");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("stat"),
            "300 (firefox) S 1 300 300 0 -1 0 0 0 0 0 10 5 0 0 20 0 1 0 1000 1000 100 \
             0 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0",
        )
        .unwrap();
        std::fs::write(d.join("cmdline"), b"firefox\0".as_slice()).unwrap();
        std::os::unix::fs::symlink("/usr/lib/firefox/firefox", d.join("exe")).unwrap();

        assert!(detect(&root, false).iter().all(|c| c.kind != Kind::TorBrowser));
    }

    /// A machine with nothing private running must report nothing, not guess.
    #[test]
    fn a_clean_system_reports_no_private_contexts() {
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        std::fs::create_dir_all(root.join("proc")).unwrap();
        std::fs::write(root.join("proc/stat"), "btime 1790097664\n").unwrap();
        // Only the real wmctrl could add rows here, and it cannot see this root.
        let found = detect(&root, false);
        assert!(found.iter().all(|c| c.kind != Kind::BraveTor));
    }

    #[test]
    fn closing_without_a_window_id_is_refused_rather_than_attempted() {
        assert!(!close(""));
    }

    #[test]
    fn kind_strings_match_what_v1_emitted_over_the_api() {
        assert_eq!(Kind::FirefoxPrivate.as_str(), "firefox_private");
        assert_eq!(Kind::BraveTor.as_str(), "brave_tor");
        assert_eq!(Kind::TorBrowser.as_str(), "tor_browser");
    }
}
