//! Window and workspace layout, via `wmctrl` and `xprop`.
//!
//! # This module is mostly disappointed on COSMIC
//!
//! It is ported faithfully, but the honest summary from v1's experience on this
//! desktop is that it rarely achieves anything:
//!
//! * `cosmic-comp` sets no `_NET_NUMBER_OF_DESKTOPS`, so `wmctrl -d` fails and
//!   the workspace count has to be inferred from the window list.
//! * `cosmic-term` and Flatpak Brave are Wayland-native and invisible to
//!   `wmctrl` entirely, so rules naming them are no-ops.
//! * COSMIC exposes no D-Bus workspace API, so there is no better path.
//!
//! It is kept because it works for XWayland clients and because deleting a
//! feature during a port is not a porting decision. Everything degrades to
//! "found nothing" rather than failing.

use crate::config::WorkspaceRule;
use crate::exec;
use crate::pattern;

/// Wayland-native clients `wmctrl` cannot see. Rules naming these are inert, and
/// saying so beats silently doing nothing.
pub const WAYLAND_NATIVE: &[&str] = &["cosmic-term", "brave", "brave-browser"];

/// One mapped window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub xid: String,
    /// `-1` means sticky (shown on every workspace).
    pub workspace: i32,
    pub pid: i32,
    pub title: String,
    /// `WM_CLASS` instance name, lowercased (e.g. `navigator`).
    pub wm_class: String,
    /// `WM_CLASS` application name, lowercased (e.g. `firefox`). This is the
    /// more reliable of the two for matching, which is why `match_type = "class"`
    /// is the config default.
    pub wm_class_app: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub n_workspaces: u32,
    pub active_workspace: i32,
    pub windows: Vec<Window>,
    /// Clients known to exist but invisible to `wmctrl`.
    pub wayland_native: Vec<String>,
}

/// Parse `wmctrl -l -p` output.
///
/// Format: `0x01000003  0 1234   hostname  Window Title`. The title is
/// everything after the hostname and may contain anything, including the
/// separators, so the split is bounded to four fields.
pub fn parse_window_list(text: &str) -> Vec<Window> {
    let mut out = Vec::new();
    for line in text.lines() {
        // `splitn` on whitespace collapses badly across runs of spaces, so take
        // the four fixed fields by whitespace and rejoin the rest as the title.
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let Ok(workspace) = fields[1].parse::<i32>() else {
            continue;
        };
        let Ok(pid) = fields[2].parse::<i32>() else {
            continue;
        };
        // Reassemble the title from the fifth field onward.
        let title = fields[4..].join(" ");
        out.push(Window {
            xid: fields[0].to_string(),
            workspace,
            pid,
            title,
            wm_class: String::new(),
            wm_class_app: String::new(),
        });
    }
    out
}

/// Parse `xprop -id <xid> WM_CLASS` into (instance, application), lowercased.
pub fn parse_wm_class(text: &str) -> (String, String) {
    let parts: Vec<&str> = text.split('"').collect();
    let instance = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
    let app = parts
        .get(3)
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| instance.clone());
    (instance, app)
}

/// Parse `wmctrl -d` into (count, active index).
///
/// Returns `None` on COSMIC, which does not set the property this reads.
pub fn parse_desktops(text: &str) -> Option<(u32, i32)> {
    let mut count = 0u32;
    let mut active = -1i32;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        let Ok(idx) = fields[0].trim_end_matches(':').parse::<i32>() else {
            continue;
        };
        count += 1;
        if fields[1] == "*" {
            active = idx;
        }
    }
    (count > 0).then_some((count, active))
}

fn wmctrl(args: &[&str]) -> Option<String> {
    let display = exec::display();
    exec::run_with_env("wmctrl", args, &[("DISPLAY", &display)])
}

/// Whether `wmctrl` is usable at all.
pub fn available() -> bool {
    wmctrl(&["-m"]).is_some()
}

/// The current layout, as far as X can see it.
pub fn layout() -> Option<Layout> {
    let list = wmctrl(&["-l", "-p"])?;
    let mut windows = parse_window_list(&list);

    let display = exec::display();
    for w in &mut windows {
        if let Some(out) =
            exec::run_with_env("xprop", &["-id", &w.xid, "WM_CLASS"], &[("DISPLAY", &display)])
        {
            let (instance, app) = parse_wm_class(&out);
            w.wm_class = instance;
            w.wm_class_app = app;
        }
    }

    // COSMIC does not set _NET_NUMBER_OF_DESKTOPS, so `wmctrl -d` fails and the
    // count is inferred from the highest workspace index actually in use.
    let (n_workspaces, active_workspace) = wmctrl(&["-d"])
        .as_deref()
        .and_then(parse_desktops)
        .unwrap_or_else(|| {
            let highest = windows.iter().map(|w| w.workspace).max().unwrap_or(0);
            ((highest.max(0) + 1) as u32, -1)
        });

    Some(Layout {
        n_workspaces,
        active_workspace,
        windows,
        wayland_native: WAYLAND_NATIVE.iter().map(|s| s.to_string()).collect(),
    })
}

/// Move one window to a workspace. Returns whether `wmctrl` accepted it.
pub fn move_window(xid: &str, workspace: i32) -> bool {
    wmctrl(&["-i", "-r", xid, "-t", &workspace.to_string()]).is_some()
}

/// What a rule would do to a window, without doing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    pub xid: String,
    pub title: String,
    pub from: i32,
    pub to: i32,
    pub matched: String,
}

/// Which windows violate the configured rules.
///
/// Separated from applying the moves so the decision can be tested without a
/// window manager, and so the UI can show what a sort *would* do.
pub fn planned_moves(windows: &[Window], rules: &[WorkspaceRule]) -> Vec<Move> {
    let mut out = Vec::new();
    for w in windows {
        // Sticky windows are on every workspace by the user's choice; moving one
        // is never what they meant.
        if w.workspace < 0 {
            continue;
        }
        for r in rules {
            let subject = match r.match_type.as_str() {
                "title" => &w.title,
                // "class" is the default and the more reliable of the two.
                _ => {
                    if pattern::matches_ci(&r.pattern, &w.wm_class_app) {
                        &w.wm_class_app
                    } else {
                        &w.wm_class
                    }
                }
            };
            let hit = pattern::matches_ci(&r.pattern, subject)
                || subject.to_lowercase().contains(&r.pattern.to_lowercase());
            if !hit {
                continue;
            }
            if w.workspace != r.workspace as i32 {
                out.push(Move {
                    xid: w.xid.clone(),
                    title: w.title.clone(),
                    from: w.workspace,
                    to: r.workspace as i32,
                    matched: r.pattern.clone(),
                });
            }
            break; // first matching rule wins
        }
    }
    out
}

/// Apply the rules, returning only the moves `wmctrl` accepted.
pub fn auto_sort(rules: &[WorkspaceRule]) -> Vec<Move> {
    let Some(l) = layout() else {
        tracing::debug!("wmctrl unavailable — workspace sorting skipped");
        return Vec::new();
    };
    planned_moves(&l.windows, rules)
        .into_iter()
        .filter(|m| move_window(&m.xid, m.to))
        .collect()
}

/// Rules that can never fire because their target is invisible to `wmctrl`.
///
/// Worth surfacing: a user who writes a `cosmic-term` rule and sees nothing
/// happen deserves to be told why rather than assuming RamWarden is broken.
pub fn inert_rules(rules: &[WorkspaceRule]) -> Vec<&WorkspaceRule> {
    rules
        .iter()
        .filter(|r| {
            WAYLAND_NATIVE
                .iter()
                .any(|n| pattern::matches_ci(&r.pattern, n) || r.pattern.eq_ignore_ascii_case(n))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WMCTRL_LP: &str = "\
0x01000003  0 1234   pop-os  Terminal — bash
0x01200005  1 5678   pop-os  Brave Browser - some page
0x01600009 -1 91011  pop-os  Sticky note
0x01800011  2 0      pop-os  no usable pid
";

    fn rule(pattern: &str, workspace: u32) -> WorkspaceRule {
        WorkspaceRule {
            pattern: pattern.to_string(),
            match_type: "class".to_string(),
            workspace,
        }
    }

    #[test]
    fn parses_the_window_list() {
        let w = parse_window_list(WMCTRL_LP);
        assert_eq!(w.len(), 4);
        assert_eq!(w[0].xid, "0x01000003");
        assert_eq!(w[0].workspace, 0);
        assert_eq!(w[0].pid, 1234);
    }

    /// Titles contain spaces, dashes, and em dashes. Splitting the whole line on
    /// whitespace and keeping only four fields would truncate every title.
    #[test]
    fn a_title_containing_spaces_survives_intact() {
        let w = parse_window_list(WMCTRL_LP);
        assert_eq!(w[0].title, "Terminal — bash");
        assert_eq!(w[1].title, "Brave Browser - some page");
    }

    #[test]
    fn a_sticky_window_is_marked_with_workspace_minus_one() {
        let w = parse_window_list(WMCTRL_LP);
        assert_eq!(w[2].workspace, -1);
    }

    #[test]
    fn malformed_and_empty_output_yields_nothing() {
        assert!(parse_window_list("").is_empty());
        assert!(parse_window_list("Cannot open display\n").is_empty());
        assert!(parse_window_list("0x1 notanumber 5 host title\n").is_empty());
    }

    #[test]
    fn parses_wm_class_into_instance_and_application() {
        let out = "WM_CLASS(STRING) = \"Navigator\", \"firefox\"\n";
        assert_eq!(parse_wm_class(out), ("navigator".into(), "firefox".into()));
    }

    #[test]
    fn wm_class_with_only_one_value_reuses_it_for_both() {
        let out = "WM_CLASS(STRING) = \"alacritty\"\n";
        assert_eq!(parse_wm_class(out), ("alacritty".into(), "alacritty".into()));
    }

    #[test]
    fn a_missing_wm_class_yields_empty_strings() {
        assert_eq!(parse_wm_class("WM_CLASS:  not found.\n"), (String::new(), String::new()));
    }

    #[test]
    fn parses_the_desktop_list_when_the_wm_provides_one() {
        let out = "0  * DG: 1920x1080  VP: 0,0  WA: 0,0 1920x1053  one\n\
                   1  - DG: 1920x1080  VP: N/A  WA: 0,0 1920x1053  two\n";
        assert_eq!(parse_desktops(out), Some((2, 0)));
    }

    /// COSMIC sets no `_NET_NUMBER_OF_DESKTOPS`, so `wmctrl -d` produces nothing
    /// usable and the count must be inferred instead.
    #[test]
    fn an_absent_desktop_list_is_none_rather_than_zero_workspaces() {
        assert_eq!(parse_desktops(""), None);
        assert_eq!(parse_desktops("Cannot get property.\n"), None);
    }

    // ── Rule evaluation ─────────────────────────────────────────────────────

    fn windows_with_classes() -> Vec<Window> {
        vec![
            Window {
                xid: "0x1".into(),
                workspace: 0,
                pid: 1,
                title: "Terminal".into(),
                wm_class: "alacritty".into(),
                wm_class_app: "alacritty".into(),
            },
            Window {
                xid: "0x2".into(),
                workspace: 2,
                pid: 2,
                title: "Firefox".into(),
                wm_class: "navigator".into(),
                wm_class_app: "firefox".into(),
            },
            Window {
                xid: "0x3".into(),
                workspace: -1,
                pid: 3,
                title: "Sticky".into(),
                wm_class: "note".into(),
                wm_class_app: "note".into(),
            },
        ]
    }

    #[test]
    fn a_window_on_the_wrong_workspace_is_planned_for_a_move() {
        let moves = planned_moves(&windows_with_classes(), &[rule("alacritty", 2)]);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].xid, "0x1");
        assert_eq!(moves[0].from, 0);
        assert_eq!(moves[0].to, 2);
    }

    #[test]
    fn a_window_already_in_place_is_left_alone() {
        let moves = planned_moves(&windows_with_classes(), &[rule("firefox", 2)]);
        assert!(moves.is_empty(), "{moves:?}");
    }

    /// A sticky window is on every workspace because the user put it there.
    #[test]
    fn a_sticky_window_is_never_moved() {
        let moves = planned_moves(&windows_with_classes(), &[rule("note", 1)]);
        assert!(moves.is_empty());
    }

    #[test]
    fn matching_on_title_is_supported() {
        let mut r = rule("Firefox", 0);
        r.match_type = "title".into();
        let moves = planned_moves(&windows_with_classes(), &[r]);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].xid, "0x2");
    }

    #[test]
    fn the_first_matching_rule_wins() {
        let moves = planned_moves(
            &windows_with_classes(),
            &[rule("alacritty", 1), rule("alacritty", 3)],
        );
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].to, 1);
    }

    #[test]
    fn no_rules_means_no_moves() {
        assert!(planned_moves(&windows_with_classes(), &[]).is_empty());
    }

    /// The config this machine ships names `cosmic-term` and `brave-browser`,
    /// neither of which `wmctrl` can see. Saying so beats silently doing nothing.
    #[test]
    fn rules_naming_wayland_native_clients_are_reported_inert() {
        let rules = vec![
            rule("cosmic-term", 2),
            rule("brave-browser", 0),
            rule("alacritty", 2),
        ];
        let inert = inert_rules(&rules);
        let names: Vec<&str> = inert.iter().map(|r| r.pattern.as_str()).collect();
        assert_eq!(names, vec!["cosmic-term", "brave-browser"]);
    }

    #[test]
    fn a_rule_for_an_xwayland_client_is_not_inert() {
        assert!(inert_rules(&[rule("gnome-terminal", 2)]).is_empty());
    }

    #[test]
    fn the_layout_degrades_rather_than_failing_when_wmctrl_is_absent() {
        // Whatever this machine has, neither call may panic.
        let _ = available();
        let _ = layout();
    }
}
