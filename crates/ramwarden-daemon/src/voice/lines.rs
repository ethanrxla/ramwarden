//! What the warden can say, read from `assets/warden/lines/<warden>.json`.
//!
//! The lines are data so they can be rewritten without touching code (product
//! spec 2.5). They are compiled in, so a missing or broken file is a build
//! failure caught by the tests below, never a silent warden at runtime.

use std::sync::OnceLock;

use serde::Deserialize;

const RAM_JSON: &str = include_str!("../../../../assets/warden/lines/ram.json");

#[derive(Debug, Deserialize)]
pub struct Lines {
    pub warden: String,
    pub check_in: Vec<String>,
    pub quips: Quips,
    pub activity: Activity,
    pub replies: Replies,
}

#[derive(Debug, Deserialize)]
pub struct Quips {
    pub any: Vec<String>,
    pub many_tabs: Vec<String>,
    pub high_memory: Vec<String>,
    pub calm: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Activity {
    pub unloaded: Vec<String>,
    pub closed: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Replies {
    pub keep: Vec<String>,
    pub close: Vec<String>,
    pub close_queued: Vec<String>,
    pub close_failed: Vec<String>,
    pub later: Vec<String>,
    pub muted: Vec<String>,
}

impl Lines {
    /// The RAM warden's lines.
    pub fn ram() -> &'static Lines {
        static LINES: OnceLock<Lines> = OnceLock::new();
        LINES.get_or_init(|| serde_json::from_str(RAM_JSON).expect("assets/warden/lines/ram.json is valid"))
    }

    /// Every line, for the checks below.
    #[cfg(test)]
    fn all(&self) -> Vec<&String> {
        let q = &self.quips;
        let r = &self.replies;
        [
            &self.check_in, &q.any, &q.many_tabs, &q.high_memory, &q.calm,
            &self.activity.unloaded, &self.activity.closed,
            &r.keep, &r.close, &r.close_queued, &r.close_failed, &r.later, &r.muted,
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// Replace `{name}` placeholders. Unknown names are left as they are, which
/// the tests below turn into a failure.
pub fn fill(line: &str, values: &[(&str, &str)]) -> String {
    let mut out = line.to_string();
    for (k, v) in values {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALUES: &[(&str, &str)] = &[
        ("title", "T"), ("idle", "3 hours"), ("tabs", "40"), ("percent", "88"),
        ("count", "4"), ("mb", "1.1 GB"),
    ];

    #[test]
    fn the_ram_lines_parse_and_every_pool_has_something_to_say() {
        let l = Lines::ram();
        assert_eq!(l.warden, "ram");
        for pool in [&l.check_in, &l.quips.any, &l.quips.many_tabs, &l.quips.high_memory, &l.quips.calm,
            &l.activity.unloaded, &l.activity.closed, &l.replies.keep, &l.replies.close,
            &l.replies.close_queued, &l.replies.close_failed, &l.replies.later, &l.replies.muted]
        {
            assert!(!pool.is_empty());
        }
    }

    #[test]
    fn every_placeholder_is_one_the_voice_fills() {
        for line in Lines::ram().all() {
            let filled = fill(line, VALUES);
            assert!(!filled.contains('{') && !filled.contains('}'), "unfilled placeholder in {line:?}");
        }
    }

    /// The site's style rule, kept in the apps too.
    #[test]
    fn no_line_uses_an_em_dash_and_none_is_long() {
        for line in Lines::ram().all() {
            assert!(!line.contains('\u{2014}'), "em dash in {line:?}");
            assert!(line.chars().count() <= 110, "too long for a bubble: {line:?}");
        }
    }

    #[test]
    fn check_ins_name_the_tab_and_how_long_it_has_been_idle() {
        for line in &Lines::ram().check_in {
            assert!(line.contains("{title}") && line.contains("{idle}"), "{line:?}");
        }
    }
}
