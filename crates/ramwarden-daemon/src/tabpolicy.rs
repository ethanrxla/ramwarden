//! Which tabs are stale enough to close.
//!
//! Ported from v1's `_heuristic_fallback`, which is the tier that runs with no
//! model at all. It stays the floor in v2: when the local model is unavailable,
//! unsure, or the GPU is busy, these rules still work.
//!
//! # The protection list is doing real work
//!
//! Every pattern here exists because closing that kind of tab loses something
//! the user cannot easily get back: a half-finished form on a dev server, an
//! open pull request, a console session, a login flow mid-redirect. A tab is
//! cheap to reopen only if it was cheap to begin with.

use crate::tabs::Tab;

/// URLs never closed automatically, whatever their age.
const NEVER_CLOSE: &[&str] = &[
    "localhost",
    "127.0.0",
    "192.168.",
    "10.0.",
    "console.",
    "dashboard.",
    "admin.",
    "accounts.google",
    "login.",
    "signin.",
];

/// Substrings marking a page with unsaved or in-progress state.
const NEVER_CLOSE_PATHS: &[&str] = &["/issues", "/pull", "/compare", "/edit"];

/// Hosts that *are* an editor. Closing one of these can lose work that was
/// never saved anywhere.
///
/// This list is not inherited from v1 — it was added because running the ported
/// heuristic against a real browser found it selecting a SharePoint Word
/// document, a SharePoint spreadsheet, two `vscode.dev` sessions and a coursework
/// chapter, all idle for two to three days. Idle is exactly what an unsaved
/// document looks like, which is why age alone cannot be the test.
const EDITOR_HOSTS: &[&str] = &[
    "sharepoint.com",
    "docs.google.com",
    "sheets.google.com",
    "slides.google.com",
    "office.com",
    "office365.com",
    "onedrive.live.com",
    "vscode.dev",
    "github.dev",
    "overleaf.com",
    "notion.so",
    "figma.com",
    "codesandbox.io",
    "stackblitz.com",
    "replit.com",
    "colab.research.google.com",
    "jupyter",
    "zybooks.com",
    "gradescope.com",
    "canvas.instructure.com",
];

/// Domains where an old tab is almost certainly forgotten rather than pending.
const STALE_DOMAINS: &[&str] = &[
    "youtube.com",
    "reddit.com",
    "twitter.com",
    "x.com",
    "instagram.com",
    "tiktok.com",
    "news.ycombinator.com",
    "medium.com",
    "substack.com",
];

/// Whether this URL must never be closed automatically.
pub fn is_protected(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else { return true; };
    if !matches!(parsed.scheme(), "http" | "https") { return true; }
    let host = parsed.host_str().unwrap_or("").trim_matches(['[',']']).to_lowercase();
    if host.is_empty() || host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") { return true; }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let private = match ip {
            std::net::IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified(),
            std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.is_unspecified() || ip.is_unique_local() || ip.is_unicast_link_local(),
        };
        if private { return true; }
    }
    if NEVER_CLOSE.iter().any(|p| host.contains(p)) || EDITOR_HOSTS.iter().any(|h| domain_matches(&host,h)) { return true; }
    let path = parsed.path().to_lowercase();
    if ["/login", "/signin", "/auth", "/oauth", "/callback", "/session"].iter().any(|p|path.starts_with(p)) { return true; }
    (domain_matches(&host,"github.com") || domain_matches(&host,"gitlab.com"))
        && NEVER_CLOSE_PATHS.iter().any(|p|path.contains(p))
}

/// Whether an old tab on this domain is likely forgotten.
fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}
pub fn is_stale_domain(url: &str) -> bool {
    url::Url::parse(url).ok().and_then(|u|u.host_str().map(str::to_owned))
        .is_some_and(|host|STALE_DOMAINS.iter().any(|d|domain_matches(&host,d)))
}

/// Tabs worth closing, most-stale first.
///
/// `threshold_minutes` comes from `[thresholds] inactivity_minutes`. Known-stale
/// domains qualify at the threshold; everything else needs twice as long, which
/// is v1's way of being assertive about forgotten media and cautious about
/// everything it does not recognise.
pub fn stale_tabs(tabs: &[Tab], threshold_minutes: i64) -> Vec<&Tab> {
    let mut out: Vec<&Tab> = tabs
        .iter()
        .filter(|t| {
            if t.inactive_minutes < threshold_minutes {
                return false;
            }
            if is_protected(&t.url) {
                return false;
            }
            if is_stale_domain(&t.url) {
                return true;
            }
            t.inactive_minutes >= threshold_minutes.saturating_mul(2)
        })
        .collect();
    out.sort_by_key(|t| std::cmp::Reverse(t.inactive_minutes));
    out
}

/// A one-line summary, in v1's wording.
pub fn summary(count: usize, pressure: f64) -> String {
    if count == 0 {
        format!("pressure at {pressure:.1}%, no stale tabs qualify under heuristic rules.")
    } else {
        format!("{count} stale tab(s) identified by rules (pressure {pressure:.1}%).")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: i64, url: &str, inactive: i64) -> Tab {
        Tab {
            id,
            url: url.to_string(),
            title: format!("tab {id}"),
            inactive_minutes: inactive,
            ..Default::default()
        }
    }

    // ── Protection ──────────────────────────────────────────────────────────

    #[test]
    fn a_dev_server_tab_is_never_closed() {
        for url in [
            "http://localhost:3000/admin",
            "http://127.0.0.1:8000/",
            "http://192.168.1.5/",
            "http://10.0.0.2:9000/",
        ] {
            assert!(is_protected(url), "{url}");
        }
    }

    #[test]
    fn an_open_pull_request_or_issue_is_never_closed() {
        assert!(is_protected("https://github.com/rust-lang/rust/pull/12345"));
        assert!(is_protected("https://github.com/me/proj/issues/7"));
        assert!(is_protected("https://gitlab.com/me/proj/issues/7"));
    }

    /// The path markers must not fire on an unrelated site, or an article at
    /// `/edit-your-life` becomes permanently unclosable.
    #[test]
    fn path_markers_only_apply_to_code_hosts() {
        assert!(!is_protected("https://blog.example.com/edit-your-life"));
        assert!(!is_protected("https://news.example.com/issues-of-our-time"));
    }

    #[test]
    fn a_console_or_login_page_is_never_closed() {
        for url in [
            "https://console.aws.amazon.com/",
            "https://dashboard.stripe.com/",
            "https://admin.example.com/",
            "https://accounts.google.com/signin",
            "https://login.microsoftonline.com/",
        ] {
            assert!(is_protected(url), "{url}");
        }
    }

    #[test]
    fn browser_internal_pages_are_never_closed() {
        for url in [
            "chrome://downloads/",
            "brave://settings",
            "about:config",
            "devtools://devtools/bundled/",
            "moz-extension://abc/page.html",
            "data:text/html,hi",
            "view-source:https://x/",
        ] {
            assert!(is_protected(url), "{url}");
        }
    }

    /// A tab whose URL the extension could not read cannot be judged, so it is
    /// not closed.
    #[test]
    fn a_tab_with_no_url_is_protected() {
        assert!(is_protected(""));
    }

    #[test]
    fn protection_is_case_insensitive() {
        assert!(is_protected("HTTP://LOCALHOST:3000/"));
        assert!(is_protected("https://GitHub.com/a/b/PULL/1"));
    }

    /// Real URLs, taken from the browser this was tested against. The ported
    /// heuristic selected every one of them for closing.
    #[test]
    fn an_online_document_or_editor_is_never_closed() {
        for url in [
            "https://fau.sharepoint.com/:w:/r/sites/QEP-LearningAssistants/_layouts/15/doc.aspx",
            "https://fau-my.sharepoint.com/:x:/r/personal/jyepes_fau_edu/_layouts/15/x.aspx",
            "https://vscode.dev/edu?projectId=b8cedd91-0c7d-4be6-997b-070f8984b828",
            "https://learn.zybooks.com/zybook/FAUCOP3410CTaebiFall2026/chapter/15/section/2",
            "https://docs.google.com/document/d/abc/edit",
            "https://www.overleaf.com/project/123",
            "https://colab.research.google.com/drive/abc",
        ] {
            assert!(is_protected(url), "{url}");
        }
    }

    /// Age is not evidence of abandonment for a document — an unsaved draft left
    /// open for three days looks exactly like a forgotten tab.
    #[test]
    fn a_three_day_old_document_is_still_not_stale() {
        let tabs = vec![tab(
            1,
            "https://fau.sharepoint.com/:w:/r/sites/QEP/_layouts/15/doc.aspx",
            4725,
        )];
        assert!(stale_tabs(&tabs, 45).is_empty());
    }

    #[test]
    fn an_ordinary_article_is_not_protected() {
        assert!(!is_protected("https://example.com/some/article"));
    }

    // ── Staleness ───────────────────────────────────────────────────────────

    #[test]
    fn a_forgotten_media_tab_qualifies_at_the_threshold() {
        let tabs = vec![tab(1, "https://www.youtube.com/watch?v=x", 45)];
        let stale = stale_tabs(&tabs, 45);
        assert_eq!(stale.len(), 1);
    }

    /// An unrecognised site needs twice as long — v1's way of being assertive
    /// about known time-sinks and cautious about everything else.
    #[test]
    fn an_unrecognised_site_needs_twice_the_threshold() {
        let tabs = vec![tab(1, "https://example.com/article", 60)];
        assert!(stale_tabs(&tabs, 45).is_empty(), "60 < 90");
        assert_eq!(stale_tabs(&tabs, 30).len(), 1, "60 >= 60");
    }

    #[test]
    fn a_recently_used_tab_is_never_stale() {
        let tabs = vec![tab(1, "https://www.youtube.com/", 10)];
        assert!(stale_tabs(&tabs, 45).is_empty());
    }

    #[test]
    fn protection_outranks_staleness_however_old_the_tab() {
        let tabs = vec![
            tab(1, "http://localhost:3000/", 99_999),
            tab(2, "https://github.com/a/b/pull/1", 99_999),
            tab(3, "chrome://downloads/", 99_999),
        ];
        assert!(stale_tabs(&tabs, 45).is_empty());
    }

    #[test]
    fn stale_tabs_come_back_most_stale_first() {
        let tabs = vec![
            tab(1, "https://reddit.com/r/a", 100),
            tab(2, "https://youtube.com/b", 500),
            tab(3, "https://x.com/c", 300),
        ];
        let ids: Vec<i64> = stale_tabs(&tabs, 45).iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![2, 3, 1]);
    }

    /// The real figures from v1's test fixtures: both well past any threshold.
    #[test]
    fn the_v1_fixture_tabs_are_both_selected() {
        let tabs = vec![
            tab(1, "https://github.com/ekomsSavior/REDflare-v2", 6087),
            tab(2, "https://labs.infoguard.ch/posts/ghost-sender/", 3346),
        ];
        assert_eq!(stale_tabs(&tabs, 45).len(), 2);
    }

    /// A GitHub *repository* page is not an open pull request.
    #[test]
    fn a_plain_repository_page_is_closeable_when_old() {
        let tabs = vec![tab(1, "https://github.com/ekomsSavior/REDflare-v2", 6087)];
        assert!(!is_protected(&tabs[0].url));
        assert_eq!(stale_tabs(&tabs, 45).len(), 1);
    }

    #[test]
    fn a_zero_threshold_does_not_make_everything_stale_at_once() {
        // Protection still applies, and the doubling is still computed safely.
        let tabs = vec![
            tab(1, "http://localhost/", 0),
            tab(2, "https://example.com/", 0),
        ];
        let stale = stale_tabs(&tabs, 0);
        assert!(!stale.iter().any(|t| t.id == 1), "localhost stays protected");
        assert_eq!(stale.len(), 1);
    }

    #[test]
    fn a_huge_threshold_does_not_overflow_when_doubled() {
        let tabs = vec![tab(1, "https://example.com/", i64::MAX)];
        // Must not panic on `threshold * 2`.
        let _ = stale_tabs(&tabs, i64::MAX);
    }

    #[test]
    fn the_summary_reads_the_way_v1_phrased_it() {
        assert!(summary(3, 72.5).contains("3 stale tab(s)"));
        assert!(summary(0, 72.5).contains("no stale tabs qualify"));
    }
}
