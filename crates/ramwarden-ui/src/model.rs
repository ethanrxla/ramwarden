//! The rows the window shows, and how they sort and filter.
//!
//! Kept free of GTK so the behaviour the user actually complained about —
//! unreadable columns, no sorting, no way to find anything — is testable without
//! a display.
//!
//! # The column widths are the whole point
//!
//! v1's window put the process list in a `ScrolledWindow` with
//! `hscrollbar_policy = NEVER` and an ellipsizing name column. The result was
//! that long names such as `app-flatpak-com.brave.Browser-1681016714.scope` or
//! even `openclaw-gatewa` rendered as `openclaw…` with no way to see the rest.
//! Every column here carries its own minimum width and an explicit statement of
//! whether it may be resized, and the view that holds them scrolls horizontally.

use serde::Deserialize;

/// One process, as the daemon reports it.
///
/// Field names match the daemon's `/state` payload, which in turn matches what
/// v1 emitted — `rss_mb` carries PSS now, but the name is unchanged because the
/// Python window still reads it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Row {
    #[serde(default)]
    pub pid: i32,
    #[serde(default)]
    pub name: String,
    /// Proportional set size in MB, despite the inherited field name.
    #[serde(default, rename = "rss_mb")]
    pub pss_mb: f64,
    /// What a naive RSS reading would have claimed.
    #[serde(default)]
    pub true_rss_mb: f64,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub protection: String,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub listening_ports: Vec<u16>,
    #[serde(default)]
    pub established: u32,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub tty: bool,
    #[serde(default)]
    pub age_minutes: f64,
    /// Filled in from `/ai`, which is the only place VRAM is reported.
    #[serde(default, skip)]
    pub vram_mb: f64,
    /// Whether the user has ticked this row.
    #[serde(default, skip)]
    pub selected: bool,
}

impl Row {
    /// The first reason, which is the headline one.
    pub fn reason(&self) -> &str {
        self.reasons.first().map(String::as_str).unwrap_or("")
    }

    /// Every reason, for the tooltip.
    pub fn all_reasons(&self) -> String {
        if self.reasons.is_empty() {
            "—".to_string()
        } else {
            self.reasons.join("\n")
        }
    }

    pub fn is_protected(&self) -> bool {
        self.verdict == "PROTECTED"
    }

    /// Structural protection cannot be overridden by any gesture in the UI.
    pub fn is_structural(&self) -> bool {
        self.protection == "structural"
    }

    pub fn is_stopped(&self) -> bool {
        self.status == "T"
    }

    /// Short scope label: the systemd scope name with its boilerplate removed.
    ///
    /// The raw names are long and nearly all prefix —
    /// `app-flatpak-com.brave.Browser-1681016714.scope` — so showing them in full
    /// wastes the width that makes the column readable in the first place.
    pub fn scope_label(&self) -> String {
        let Some(s) = &self.scope else {
            return String::new();
        };
        s.trim_end_matches(".scope")
            .trim_start_matches("app-flatpak-")
            .trim_start_matches("app-cosmic-")
            .trim_start_matches("app-org.")
            .trim_start_matches("app-")
            .to_string()
    }

    /// How far a naive RSS reading would overstate this process.
    pub fn overstatement(&self) -> f64 {
        if self.pss_mb <= 0.0 {
            return 1.0;
        }
        self.true_rss_mb / self.pss_mb
    }

    /// Whether any UI action beyond "look at it" is permitted.
    pub fn actionable(&self) -> bool {
        !self.is_structural()
    }
}

/// Which column the view is sorted by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SortKey {
    Name,
    Pid,
    #[default]
    Memory,
    Vram,
    Status,
    Scope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Direction {
    Ascending,
    #[default]
    Descending,
}

impl Direction {
    pub fn flipped(self) -> Self {
        match self {
            Direction::Ascending => Direction::Descending,
            Direction::Descending => Direction::Ascending,
        }
    }
}

/// One column of the table.
pub struct Column {
    pub title: &'static str,
    /// Minimum width in pixels. Chosen so the longest realistic value fits
    /// rather than ellipsizing — the complaint this table exists to fix.
    pub min_width: i32,
    /// Whether the user may drag its edge.
    pub resizable: bool,
    /// Whether clicking the header sorts by it.
    pub sort: Option<SortKey>,
    /// Whether it takes leftover width.
    pub expand: bool,
}

/// The table's columns, in order.
///
/// `Process` is both resizable and wide by default because it is the column that
/// was unreadable: 260px fits `openclaw-gatewa` and most Flatpak scope stems, and
/// dragging covers the rest.
pub const COLUMNS: &[Column] = &[
    Column { title: "",        min_width: 34,  resizable: false, sort: None,                   expand: false },
    Column { title: "Process", min_width: 260, resizable: true,  sort: Some(SortKey::Memory),  expand: true  },
    Column { title: "PID",     min_width: 80,  resizable: true,  sort: Some(SortKey::Pid),     expand: false },
    Column { title: "RAM",     min_width: 100, resizable: true,  sort: Some(SortKey::Memory),  expand: false },
    Column { title: "VRAM",    min_width: 90,  resizable: true,  sort: Some(SortKey::Vram),    expand: false },
    Column { title: "Status",  min_width: 110, resizable: true,  sort: Some(SortKey::Status),  expand: false },
    Column { title: "Scope",   min_width: 180, resizable: true,  sort: Some(SortKey::Scope),   expand: false },
    Column { title: "Why",     min_width: 320, resizable: true,  sort: None,                   expand: true  },
];

/// Colours for each verdict, carried over from v1's legend so the window looks
/// familiar to someone who used it before.
pub fn verdict_colour(verdict: &str) -> &'static str {
    match verdict {
        "PROTECTED" => "#60a5fa", // blue — structurally off-limits
        "IN_USE" => "#4ade80",    // green — you are using it
        "IDLE" => "#facc15",      // amber — reclaimable
        _ => "#9ca3af",
    }
}

/// Sort rows in place.
pub fn sort(rows: &mut [Row], key: SortKey, dir: Direction) {
    rows.sort_by(|a, b| {
        let ord = match key {
            // Names compare case-insensitively, or `ChatGPT` and `brave` sort by
            // ASCII case rather than alphabetically.
            SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortKey::Pid => a.pid.cmp(&b.pid),
            SortKey::Memory => a.pss_mb.total_cmp(&b.pss_mb),
            SortKey::Vram => a.vram_mb.total_cmp(&b.vram_mb),
            SortKey::Status => a.verdict.cmp(&b.verdict),
            SortKey::Scope => a.scope_label().to_lowercase().cmp(&b.scope_label().to_lowercase()),
        };
        // A stable tiebreak on pid, so rows do not shuffle between refreshes
        // when their sort values are equal.
        let ord = ord.then_with(|| a.pid.cmp(&b.pid));
        match dir {
            Direction::Ascending => ord,
            Direction::Descending => ord.reverse(),
        }
    });
}

/// Whether a row matches the filter box.
///
/// Matches on name, pid, scope and verdict, case-insensitively. Searching a pid
/// matters: the ladder's log and the daemon's API both speak in pids, so finding
/// one by number is how a user connects the two.
pub fn matches(row: &Row, filter: &str) -> bool {
    let f = filter.trim().to_lowercase();
    if f.is_empty() {
        return true;
    }
    row.name.to_lowercase().contains(&f)
        || row.pid.to_string().contains(&f)
        || row.scope_label().to_lowercase().contains(&f)
        || row.verdict.to_lowercase().contains(&f)
        || row.role.to_lowercase().contains(&f)
}

/// Apply a filter, preserving order.
pub fn filter(rows: &[Row], text: &str) -> Vec<Row> {
    rows.iter().filter(|r| matches(r, text)).cloned().collect()
}

/// Format a megabyte figure the way v1's window did.
pub fn fmt_mb(mb: f64) -> String {
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

/// An action the context menu can offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Ask the kernel to page the application's cold memory to zram. Invisible
    /// to the app, and the first thing to try.
    Reclaim,
    Suspend,
    Resume,
    Kill,
    /// Add to the watchlist, making it reclaimable automatically.
    Watch,
    /// Remove from the watchlist.
    Spare,
    CopyPid,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Reclaim => "Reclaim cold memory",
            Action::Suspend => "Suspend",
            Action::Resume => "Resume",
            Action::Kill => "Kill",
            Action::Watch => "Add to watchlist",
            Action::Spare => "Remove from watchlist",
            Action::CopyPid => "Copy PID",
        }
    }

    /// Whether this action destroys anything the user has not saved.
    pub fn destructive(self) -> bool {
        matches!(self, Action::Kill)
    }
}

/// One entry in the context menu.
pub struct MenuItem {
    pub action: Action,
    /// The consequence, when there is one the user should see before clicking.
    ///
    /// This exists because of a row observed on the real machine:
    /// `openclaw-gatewa`, listening on three ports, verdict `PROTECTED` with
    /// `protection: "serving"`. That protection is *soft* — the daemon's gate
    /// lets an explicit, forced action through — so the menu legitimately offers
    /// Suspend and Kill. Offering them silently would mean a right-click
    /// stranding every client on those sockets with no warning at all.
    pub warning: Option<String>,
}

impl MenuItem {
    fn plain(action: Action) -> Self {
        MenuItem {
            action,
            warning: None,
        }
    }

    pub fn label(&self) -> &'static str {
        self.action.label()
    }

    /// Whether clicking this should ask first.
    pub fn needs_confirmation(&self) -> bool {
        self.action.destructive() || self.warning.is_some()
    }
}

/// Which actions make sense for a row, and what to warn about.
///
/// Structural protection removes everything but inspection: there is no gesture
/// in this window that should be able to freeze the compositor, and offering a
/// menu item the daemon will refuse is worse than not offering it.
pub fn menu_for(row: &Row, watchlisted: bool) -> Vec<MenuItem> {
    if row.is_structural() {
        return vec![MenuItem::plain(Action::CopyPid)];
    }

    // A soft-protected row is still reachable, but the reason it was protected
    // has to travel with the offer.
    let serving_warning = || -> Option<String> {
        if row.protection != "serving" {
            return None;
        }
        Some(if row.listening_ports.is_empty() {
            "this process is serving — clients would hang".to_string()
        } else {
            let ports: Vec<String> = row
                .listening_ports
                .iter()
                .take(4)
                .map(|p| p.to_string())
                .collect();
            format!("serving on port {} — clients would hang", ports.join(", "))
        })
    };

    let mut out = Vec::new();
    if row.is_stopped() {
        // A stopped process cannot usefully be reclaimed from or suspended
        // again; the only thing to do is wake it.
        out.push(MenuItem::plain(Action::Resume));
    } else {
        // Reclaim is non-destructive and invisible to the application, so it
        // carries no warning even for a server.
        out.push(MenuItem::plain(Action::Reclaim));
        out.push(MenuItem {
            action: Action::Suspend,
            warning: serving_warning(),
        });
    }
    out.push(MenuItem {
        action: Action::Kill,
        warning: serving_warning(),
    });
    out.push(MenuItem::plain(if watchlisted {
        Action::Spare
    } else {
        Action::Watch
    }));
    out.push(MenuItem::plain(Action::CopyPid));
    out
}

/// Just the actions, for callers that do not render warnings.
pub fn actions_for(row: &Row, watchlisted: bool) -> Vec<Action> {
    menu_for(row, watchlisted)
        .into_iter()
        .map(|i| i.action)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: i32, name: &str, pss: f64, verdict: &str) -> Row {
        Row {
            pid,
            name: name.into(),
            pss_mb: pss,
            true_rss_mb: pss * 1.3,
            status: "S".into(),
            verdict: verdict.into(),
            protection: if verdict == "PROTECTED" { "structural".into() } else { String::new() },
            reasons: vec!["no CPU since the last sample".into()],
            ..Default::default()
        }
    }

    fn sample() -> Vec<Row> {
        vec![
            row(6669, "brave", 5160.0, "IDLE"),
            row(4886, "cosmic-comp", 1531.0, "PROTECTED"),
            row(89406, "ChatGPT", 816.0, "IDLE"),
            row(245567, "Discord", 476.0, "IN_USE"),
        ]
    }

    // ── The complaint this table exists to fix ──────────────────────────────

    /// v1 ellipsized the process name with no way to widen or scroll. Every
    /// column must declare a width that fits real values, and the ones holding
    /// long text must be resizable.
    #[test]
    fn every_data_column_is_resizable_and_wide_enough_to_read() {
        for c in COLUMNS.iter().filter(|c| !c.title.is_empty()) {
            assert!(c.resizable, "{} must be draggable", c.title);
            assert!(c.min_width >= 80, "{} is too narrow at {}px", c.title, c.min_width);
        }
    }

    /// The three columns the user named — Process, RAM, Status — must all exist
    /// and all be adjustable.
    #[test]
    fn the_columns_the_user_asked_about_are_present_and_adjustable() {
        for title in ["Process", "RAM", "Status"] {
            let c = COLUMNS
                .iter()
                .find(|c| c.title == title)
                .unwrap_or_else(|| panic!("no {title} column"));
            assert!(c.resizable, "{title} must be resizable");
            assert!(c.sort.is_some(), "{title} must be sortable");
        }
    }

    /// The name column was the unreadable one, so it gets the width and the
    /// leftover space.
    #[test]
    fn the_process_column_is_the_widest_and_expands() {
        let p = COLUMNS.iter().find(|c| c.title == "Process").unwrap();
        assert!(p.expand);
        assert!(p.min_width >= 260, "{}px will ellipsize real names", p.min_width);
        // Long enough for the values that were being truncated.
        assert!("openclaw-gatewa".len() * 9 < p.min_width as usize);
    }

    #[test]
    fn the_checkbox_column_is_first_narrow_and_not_sortable() {
        let first = &COLUMNS[0];
        assert_eq!(first.title, "");
        assert!(!first.resizable, "a checkbox needs no dragging");
        assert!(first.sort.is_none());
        assert!(first.min_width < 50);
    }

    /// Something has to be wide enough to show the reason, or the window tells
    /// the user a verdict without telling them why.
    #[test]
    fn the_reason_column_is_wide_and_expands() {
        let w = COLUMNS.iter().find(|c| c.title == "Why").unwrap();
        assert!(w.expand);
        assert!(w.min_width >= 300, "reasons are sentences: {}px", w.min_width);
        let longest = "desktop compositor — suspending it freezes the session";
        assert!(longest.len() * 7 < w.min_width as usize * 2);
    }

    // ── Sorting ─────────────────────────────────────────────────────────────

    #[test]
    fn sorting_by_memory_descending_puts_the_biggest_first() {
        let mut rows = sample();
        sort(&mut rows, SortKey::Memory, Direction::Descending);
        assert_eq!(rows[0].name, "brave");
        assert_eq!(rows[3].name, "Discord");
    }

    #[test]
    fn sorting_by_memory_ascending_reverses_it() {
        let mut rows = sample();
        sort(&mut rows, SortKey::Memory, Direction::Ascending);
        assert_eq!(rows[0].name, "Discord");
        assert_eq!(rows[3].name, "brave");
    }

    /// `ChatGPT` and `brave` must sort alphabetically, not by ASCII case —
    /// otherwise every capitalised name clusters at one end.
    #[test]
    fn sorting_by_name_ignores_case() {
        let mut rows = sample();
        sort(&mut rows, SortKey::Name, Direction::Ascending);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["brave", "ChatGPT", "cosmic-comp", "Discord"]);
    }

    #[test]
    fn sorting_by_pid_is_numeric_not_lexical() {
        let mut rows = sample();
        sort(&mut rows, SortKey::Pid, Direction::Ascending);
        let pids: Vec<i32> = rows.iter().map(|r| r.pid).collect();
        assert_eq!(pids, vec![4886, 6669, 89406, 245567]);
    }

    /// Rows must not shuffle between refreshes when their sort values tie, or the
    /// table jitters while the user is trying to click something.
    #[test]
    fn equal_values_tiebreak_stably_on_pid() {
        let mut rows = vec![
            row(300, "same", 100.0, "IDLE"),
            row(100, "same", 100.0, "IDLE"),
            row(200, "same", 100.0, "IDLE"),
        ];
        sort(&mut rows, SortKey::Memory, Direction::Descending);
        let first = rows.iter().map(|r| r.pid).collect::<Vec<_>>();
        sort(&mut rows, SortKey::Memory, Direction::Descending);
        assert_eq!(first, rows.iter().map(|r| r.pid).collect::<Vec<_>>());
    }

    #[test]
    fn sorting_never_panics_on_a_nan_memory_figure() {
        let mut rows = vec![row(1, "a", f64::NAN, "IDLE"), row(2, "b", 5.0, "IDLE")];
        sort(&mut rows, SortKey::Memory, Direction::Descending);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn direction_flips() {
        assert_eq!(Direction::Ascending.flipped(), Direction::Descending);
        assert_eq!(Direction::Descending.flipped(), Direction::Ascending);
        assert_eq!(Direction::default(), Direction::Descending, "biggest first");
    }

    // ── Filtering ───────────────────────────────────────────────────────────

    #[test]
    fn an_empty_filter_keeps_everything() {
        assert_eq!(filter(&sample(), "").len(), 4);
        assert_eq!(filter(&sample(), "   ").len(), 4);
    }

    #[test]
    fn filtering_matches_the_name_case_insensitively() {
        assert_eq!(filter(&sample(), "BRAVE").len(), 1);
        assert_eq!(filter(&sample(), "chat").len(), 1);
    }

    /// The ladder's log and the daemon's API both speak in pids, so finding one
    /// by number is how a user connects what they read to what they see.
    #[test]
    fn filtering_matches_a_pid() {
        let found = filter(&sample(), "89406");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "ChatGPT");
        // A partial pid works too.
        assert_eq!(filter(&sample(), "488").len(), 1);
    }

    #[test]
    fn filtering_matches_a_verdict_so_idle_rows_can_be_isolated() {
        assert_eq!(filter(&sample(), "idle").len(), 2);
        assert_eq!(filter(&sample(), "protected").len(), 1);
    }

    #[test]
    fn filtering_matches_the_scope() {
        let mut rows = sample();
        rows[0].scope = Some("app-flatpak-com.brave.Browser-1681016714.scope".into());
        assert_eq!(filter(&rows, "Browser-168").len(), 1);
    }

    #[test]
    fn a_filter_matching_nothing_yields_nothing_rather_than_everything() {
        assert!(filter(&sample(), "zzz-no-such-process").is_empty());
    }

    // ── Scope labels ────────────────────────────────────────────────────────

    /// Raw scope names are almost all boilerplate, and showing them in full
    /// wastes the width that makes the column readable.
    #[test]
    fn scope_labels_drop_the_boilerplate() {
        let mut r = row(1, "brave", 1.0, "IDLE");
        r.scope = Some("app-flatpak-com.brave.Browser-1681016714.scope".into());
        assert_eq!(r.scope_label(), "com.brave.Browser-1681016714");

        r.scope = Some("app-cosmic-com.system76.CosmicAppList-88887.scope".into());
        assert_eq!(r.scope_label(), "com.system76.CosmicAppList-88887");

        r.scope = Some("openclaw-gateway.service".into());
        assert_eq!(r.scope_label(), "openclaw-gateway.service");
    }

    #[test]
    fn a_process_outside_the_session_has_no_scope_label() {
        assert_eq!(row(1, "x", 1.0, "IDLE").scope_label(), "");
    }

    // ── Context menu ────────────────────────────────────────────────────────

    /// There is no gesture in this window that should be able to freeze the
    /// compositor, and offering an item the daemon will refuse is worse than
    /// leaving it out.
    #[test]
    fn a_structurally_protected_row_offers_nothing_but_inspection() {
        let comp = row(4886, "cosmic-comp", 1531.0, "PROTECTED");
        assert!(comp.is_structural());
        assert_eq!(actions_for(&comp, false), vec![Action::CopyPid]);
        assert!(!comp.actionable());
    }

    #[test]
    fn an_ordinary_row_offers_reclaim_before_anything_destructive() {
        let acts = actions_for(&row(1, "Discord", 476.0, "IDLE"), false);
        assert_eq!(acts[0], Action::Reclaim, "the cheap action comes first");
        let kill_at = acts.iter().position(|a| *a == Action::Kill).unwrap();
        let reclaim_at = acts.iter().position(|a| *a == Action::Reclaim).unwrap();
        assert!(reclaim_at < kill_at);
    }

    /// Reclaiming from or suspending an already-stopped process does nothing
    /// useful; waking it is the only sensible offer.
    #[test]
    fn a_stopped_row_offers_resume_instead_of_suspend() {
        let mut r = row(1, "Discord", 476.0, "IDLE");
        r.status = "T".into();
        let acts = actions_for(&r, true);
        assert!(acts.contains(&Action::Resume));
        assert!(!acts.contains(&Action::Suspend));
        assert!(!acts.contains(&Action::Reclaim));
    }

    #[test]
    fn the_watchlist_item_reflects_current_membership() {
        let r = row(1, "Discord", 476.0, "IDLE");
        assert!(actions_for(&r, false).contains(&Action::Watch));
        assert!(actions_for(&r, true).contains(&Action::Spare));
        assert!(!actions_for(&r, true).contains(&Action::Watch));
    }

    /// The row that exposed this: listening on three ports, soft-protected, and
    /// the menu offered Suspend and Kill with no warning at all.
    #[test]
    fn a_serving_row_warns_before_suspend_and_kill() {
        let mut r = row(1833243, "openclaw-gatewa", 442.0, "IN_USE");
        r.protection = "serving".into();
        r.listening_ports = vec![18789, 18791, 37059];

        let menu = menu_for(&r, false);
        let suspend = menu.iter().find(|i| i.action == Action::Suspend).unwrap();
        let kill = menu.iter().find(|i| i.action == Action::Kill).unwrap();

        for item in [suspend, kill] {
            let w = item.warning.as_deref().unwrap_or("");
            assert!(w.contains("18789"), "{w}");
            assert!(w.contains("clients would hang"), "{w}");
            assert!(item.needs_confirmation());
        }
    }

    /// Reclaim is invisible to the application, so a server needs no warning for
    /// it — warning about everything is the same as warning about nothing.
    #[test]
    fn reclaim_carries_no_warning_even_for_a_server() {
        let mut r = row(1, "my-api", 400.0, "IN_USE");
        r.protection = "serving".into();
        r.listening_ports = vec![8000];
        let menu = menu_for(&r, false);
        let reclaim = menu.iter().find(|i| i.action == Action::Reclaim).unwrap();
        assert!(reclaim.warning.is_none());
        assert!(!reclaim.needs_confirmation());
    }

    #[test]
    fn an_ordinary_row_warns_only_about_kill() {
        let r = row(1, "Discord", 476.0, "IDLE");
        for item in menu_for(&r, false) {
            match item.action {
                Action::Kill => assert!(item.needs_confirmation(), "kill must confirm"),
                _ => assert!(item.warning.is_none(), "{:?} should be quiet", item.action),
            }
        }
    }

    #[test]
    fn a_serving_row_with_no_known_ports_still_warns() {
        let mut r = row(1, "my-api", 400.0, "IN_USE");
        r.protection = "serving".into();
        let menu = menu_for(&r, false);
        let kill = menu.iter().find(|i| i.action == Action::Kill).unwrap();
        assert!(kill.warning.as_deref().unwrap().contains("serving"));
    }

    #[test]
    fn a_structural_row_has_no_menu_items_that_need_confirming() {
        let comp = row(4886, "cosmic-comp", 2700.0, "PROTECTED");
        let menu = menu_for(&comp, false);
        assert_eq!(menu.len(), 1);
        assert!(!menu[0].needs_confirmation());
        assert_eq!(menu[0].action, Action::CopyPid);
    }

    #[test]
    fn only_kill_is_marked_destructive() {
        assert!(Action::Kill.destructive());
        for a in [Action::Reclaim, Action::Suspend, Action::Resume, Action::Watch, Action::Spare, Action::CopyPid] {
            assert!(!a.destructive(), "{:?}", a);
        }
    }

    #[test]
    fn every_action_has_a_label_a_user_can_read() {
        for a in [Action::Reclaim, Action::Suspend, Action::Resume, Action::Kill, Action::Watch, Action::Spare, Action::CopyPid] {
            assert!(a.label().len() > 3, "{:?}", a);
        }
    }

    // ── Formatting and parsing ──────────────────────────────────────────────

    #[test]
    fn megabytes_format_the_way_v1s_window_did() {
        assert_eq!(fmt_mb(476.0), "476 MB");
        assert_eq!(fmt_mb(5160.0), "5.0 GB");
        assert_eq!(fmt_mb(1024.0), "1.0 GB");
        assert_eq!(fmt_mb(0.0), "0 MB");
    }

    #[test]
    fn verdict_colours_match_v1s_legend() {
        assert_eq!(verdict_colour("PROTECTED"), "#60a5fa");
        assert_eq!(verdict_colour("IN_USE"), "#4ade80");
        assert_eq!(verdict_colour("IDLE"), "#facc15");
        assert_ne!(verdict_colour("something new"), "");
    }

    #[test]
    fn a_row_parses_from_the_daemons_state_payload() {
        let r: Row = serde_json::from_str(
            r#"{"pid":6669,"name":"brave","rss_mb":5160.2,"true_rss_mb":12160.0,
                "status":"S","role":"BROWSER","verdict":"IDLE","protection":"",
                "reasons":["no CPU since the last sample"],
                "scope":"app-flatpak-com.brave.Browser-1681016714.scope",
                "listening_ports":[9222],"established":10,"focused":false,
                "audio":false,"tty":false,"age_minutes":600.0}"#,
        )
        .unwrap();
        assert_eq!(r.pid, 6669);
        assert_eq!(r.pss_mb, 5160.2);
        assert_eq!(r.listening_ports, vec![9222]);
        assert_eq!(r.scope_label(), "com.brave.Browser-1681016714");
        assert_eq!(r.reason(), "no CPU since the last sample");
    }

    #[test]
    fn a_row_with_missing_fields_still_parses() {
        let r: Row = serde_json::from_str(r#"{"pid":1,"name":"x"}"#).unwrap();
        assert_eq!(r.pss_mb, 0.0);
        assert_eq!(r.all_reasons(), "—");
        assert!(r.scope.is_none());
    }

    /// The table can show the user exactly how wrong v1 was about their memory.
    #[test]
    fn overstatement_is_computed_from_both_figures() {
        let r = Row {
            pss_mb: 6512.0,
            true_rss_mb: 15094.0,
            ..Default::default()
        };
        assert!((r.overstatement() - 2.318).abs() < 0.01);
        assert_eq!(Row::default().overstatement(), 1.0, "no divide by zero");
    }
}
