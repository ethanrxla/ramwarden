//! Plain-language labels for processes.
//!
//! A desktop app is rarely one process. Chrome, Claude and ChatGPT each run
//! tens of helpers whose names (`Google Chrome Helper (Aperitif Renderer)`) say
//! nothing, and they are where the memory goes. This module turns a name and a
//! command line into *which app* a process belongs to and *what part of it* it
//! is, so the UI can say "Claude: page renderer, 700 MB, idle 40 min" instead of
//! a bare pid. Everything here is pure string work, so it is exact and tested.

use crate::signals::Signals;

/// The application a process belongs to: the first `Something.app` directory in
/// its executable path, else the process name.
pub fn app_of(name: &str, cmdline: &str) -> String {
    if let Some(end) = cmdline.find(".app/") {
        let head = &cmdline[..end];
        if let Some(start) = head.rfind('/') {
            let app = &head[start + 1..];
            if !app.is_empty() {
                return app.to_string();
            }
        }
    }
    name.to_string()
}

/// Which part of an application this process is. Chromium and Electron mark
/// their children on the command line (`--type=`, `--utility-sub-type=`).
pub fn part_of(name: &str, cmdline: &str) -> &'static str {
    let flag = |f: &str| cmdline.split_whitespace().find_map(|w| w.strip_prefix(f));
    match flag("--type=") {
        Some("renderer") => {
            if cmdline.contains("--extension-process") {
                "extension page renderer"
            } else {
                "page renderer"
            }
        }
        Some("gpu-process") => "graphics process",
        Some("zygote") => "process spawner",
        Some("utility") => match flag("--utility-sub-type=").unwrap_or("") {
            s if s.starts_with("network") => "network service",
            s if s.starts_with("storage") => "storage service",
            s if s.starts_with("audio") => "audio service",
            s if s.starts_with("video_capture") => "camera service",
            s if s.starts_with("node") => "background node service",
            s if s.contains("sandbox") => "sandboxed worker",
            _ => "utility worker",
        },
        Some(_) => "helper",
        None => {
            let n = name.to_lowercase();
            if n.contains("language_server") || n.contains("language-server") || n.contains("tsserver") {
                "language server"
            } else if n.contains("extension-host") || cmdline.contains("chrome-extension://") {
                "browser extension host"
            } else if n.contains("helper") {
                "helper"
            } else {
                "main process"
            }
        }
    }
}

fn idle_text(minutes: f64) -> String {
    if minutes < 1.0 {
        "under a minute".into()
    } else if minutes < 90.0 {
        format!("{:.0} min", minutes)
    } else {
        format!("{:.1} h", minutes / 60.0)
    }
}

/// One line a person can act on: app, part, size and what it is doing.
///
/// `quiet_minutes` is how long the process has been quiet; the detector only
/// knows the last sample, so callers that track longer idleness pass it in.
pub fn label(s: &Signals, quiet_minutes: Option<f64>) -> String {
    let mb = (if s.rss > 0 { s.rss } else { s.pss }) as f64 / (1024.0 * 1024.0);
    let doing = if s.is_focused {
        "in the app you are using now".to_string()
    } else if s.playing_audio {
        "playing or recording audio".to_string()
    } else if let Some(d) = &s.active_descendant {
        format!("a child process is working ({d})")
    } else if !s.listening_ports.is_empty() {
        let p: Vec<String> = s.listening_ports.iter().take(3).map(|p| p.to_string()).collect();
        format!("listening on port {}", p.join(", "))
    } else if s.cpu_seconds_recent >= crate::signals::CPU_BUSY_SECONDS {
        "working right now".to_string()
    } else {
        match quiet_minutes {
            Some(m) => format!("no CPU for {}", idle_text(m)),
            None => "no CPU since the last check".to_string(),
        }
    };
    let window = if s.has_window { "has a window" } else { "no window" };
    format!("{} - {}: {:.0} MB, {doing}, {window}", s.app, s.part, mb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::classify;

    const CHROME_RENDERER: &str = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/155/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer) --type=renderer --lang=en-US";

    #[test]
    fn the_app_is_the_outermost_bundle_not_the_helper() {
        assert_eq!(app_of("Google Chrome Helper (Renderer)", CHROME_RENDERER), "Google Chrome");
        assert_eq!(app_of("ChatGPTHelper", "Contents/Resources/ChatGPTHelper"), "ChatGPTHelper");
        assert_eq!(app_of("node", "node server.js"), "node");
    }

    #[test]
    fn chromium_and_electron_children_are_named_by_what_they_do() {
        assert_eq!(part_of("x", CHROME_RENDERER), "page renderer");
        assert_eq!(part_of("x", "h --type=renderer --extension-process"), "extension page renderer");
        assert_eq!(part_of("x", "h --type=gpu-process --x"), "graphics process");
        assert_eq!(part_of("x", "h --type=utility --utility-sub-type=network.mojom.NetworkService"), "network service");
        assert_eq!(part_of("x", "h --type=utility --utility-sub-type=node.mojom.NodeService"), "background node service");
        assert_eq!(part_of("x", "h --type=utility --utility-sub-type=audio.mojom.AudioService"), "audio service");
        assert_eq!(part_of("Claude", "/Applications/Claude.app/Contents/MacOS/Claude"), "main process");
        assert_eq!(part_of("language_server_macos_arm", "x --enable_lsp"), "language server");
        assert_eq!(part_of("ChatGPT for Chrome", "ChatGPT for Chrome chrome-extension://abc/"), "browser extension host");
    }

    fn sig(app: &str, part: &'static str) -> Signals {
        let mut s = Signals::new(1, "x", classify("x", "", None));
        s.app = app.into();
        s.part = part;
        s.rss = 700 * 1024 * 1024;
        s
    }

    #[test]
    fn a_label_says_who_what_how_big_and_whether_it_is_doing_anything() {
        let s = sig("Claude", "page renderer");
        assert_eq!(label(&s, Some(40.0)), "Claude - page renderer: 700 MB, no CPU for 40 min, no window");
        let mut w = sig("Claude", "main process");
        w.has_window = true;
        w.active_descendant = Some("codex".into());
        assert!(label(&w, None).contains("a child process is working (codex)"));
        let mut srv = sig("Discord", "helper");
        srv.listening_ports = vec![6463];
        assert!(label(&srv, None).contains("listening on port 6463"));
    }

    #[test]
    fn long_idleness_is_shown_in_hours() {
        assert_eq!(idle_text(30.0), "30 min");
        assert_eq!(idle_text(150.0), "2.5 h");
    }
}
