//! Shell-style name matching, equivalent to Python's `fnmatch`.
//!
//! The watchlist in `ramwarden.toml` is written by hand and has always accepted
//! globs (`burp*` catching both `burpsuite` and `BurpSuiteCommunity`). Porting
//! the config format unchanged means porting this matcher too.
//!
//! Matching is case-sensitive here; callers lower-case both sides, which is what
//! v1 did and what makes a `Discord` entry match a `discord` process.

/// Whether `text` matches shell-style `pattern` (`*`, `?`, `[abc]`, `[!abc]`).
pub fn matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    is_match(&p, &t)
}

/// Case-insensitive match, the form every caller in RamWarden actually wants.
pub fn matches_ci(pattern: &str, text: &str) -> bool {
    matches(&pattern.to_lowercase(), &text.to_lowercase())
}

/// Whether `text` matches any pattern in `patterns`, case-insensitively.
pub fn any_ci(patterns: &[String], text: &str) -> bool {
    patterns.iter().any(|p| matches_ci(p, text))
}

fn is_match(p: &[char], t: &[char]) -> bool {
    // Iterative with one backtrack point, so a pathological pattern like
    // `*a*a*a*` cannot blow the stack or the clock.
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;

    while ti < t.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some((pi, ti));
                    pi += 1;
                    continue;
                }
                '?' => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                '[' => {
                    if let Some((end, ok)) = class_match(p, pi, t[ti]) {
                        if ok {
                            pi = end + 1;
                            ti += 1;
                            continue;
                        }
                    } else if p[pi] == t[ti] {
                        // Unterminated '[' is a literal, as fnmatch treats it.
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
                c if c == t[ti] => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                _ => {}
            }
        }

        // Mismatch: resume after the last '*', consuming one more character.
        match star {
            Some((sp, st)) => {
                pi = sp + 1;
                ti = st + 1;
                star = Some((sp, st + 1));
            }
            None => return false,
        }
    }

    // Trailing '*'s may match the empty remainder.
    p[pi..].iter().all(|&c| c == '*')
}

/// Match one `[...]` class against `c`, returning the closing index and result.
fn class_match(p: &[char], open: usize, c: char) -> Option<(usize, bool)> {
    let mut i = open + 1;
    let negated = matches!(p.get(i), Some('!') | Some('^'));
    if negated {
        i += 1;
    }
    let first = i;
    let mut hit = false;

    while i < p.len() {
        // A ']' in the first position is a literal, per POSIX.
        if p[i] == ']' && i > first {
            return Some((i, hit != negated));
        }
        // A range, unless the '-' is first or last in the class.
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            if p[i] <= c && c <= p[i + 2] {
                hit = true;
            }
            i += 3;
            continue;
        }
        if p[i] == c {
            hit = true;
        }
        i += 1;
    }
    None // unterminated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_names_match() {
        assert!(matches("Discord", "Discord"));
        assert!(!matches("Discord", "Discord2"));
        assert!(!matches("Discord", "discord"));
    }

    #[test]
    fn star_matches_any_run() {
        assert!(matches("burp*", "burpsuite"));
        assert!(matches("*suite", "burpsuite"));
        assert!(matches("*", "anything"));
        assert!(matches("*", ""));
        assert!(matches("b*e", "burpsuite"));
        assert!(!matches("burp*", "jsuite"));
    }

    #[test]
    fn question_mark_matches_exactly_one() {
        assert!(matches("cc?", "cc1"));
        assert!(!matches("cc?", "cc"));
        assert!(!matches("cc?", "cc1plus"));
    }

    #[test]
    fn character_classes_work() {
        assert!(matches("cc[12]", "cc1"));
        assert!(matches("cc[12]", "cc2"));
        assert!(!matches("cc[12]", "cc3"));
        assert!(matches("cc[0-9]", "cc7"));
        assert!(!matches("cc[0-9]", "ccx"));
    }

    #[test]
    fn negated_classes_work() {
        assert!(matches("cc[!0-9]", "ccx"));
        assert!(!matches("cc[!0-9]", "cc1"));
        assert!(matches("cc[^0-9]", "ccx"));
    }

    #[test]
    fn case_insensitive_matching_is_what_the_watchlist_uses() {
        assert!(matches_ci("discord", "Discord"));
        assert!(matches_ci("BURP*", "burpsuite"));
        assert!(matches_ci("Discord", "DISCORD"));
    }

    #[test]
    fn any_ci_checks_a_whole_watchlist() {
        let wl = vec!["Discord".to_string(), "burp*".to_string()];
        assert!(any_ci(&wl, "discord"));
        assert!(any_ci(&wl, "BurpSuiteCommunity"));
        assert!(!any_ci(&wl, "brave"));
        assert!(!any_ci(&[], "anything"));
    }

    /// The real v1 watchlist, which must keep behaving identically.
    #[test]
    fn the_shipped_watchlist_still_matches_what_it_did_in_v1() {
        let wl: Vec<String> = ["Discord", "BurpSuiteCommunity", "burpsuite"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(any_ci(&wl, "Discord"));
        assert!(any_ci(&wl, "burpsuite"));
        assert!(any_ci(&wl, "BurpSuiteCommunity"));
        assert!(!any_ci(&wl, "cosmic-comp"));
        assert!(!any_ci(&wl, "qemu-system-x86_64"));
    }

    #[test]
    fn a_pathological_pattern_terminates() {
        // Naive recursion goes exponential here; the single backtrack point does not.
        assert!(!matches(
            "*a*a*a*a*a*a*a*a*b",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
    }

    #[test]
    fn an_unterminated_class_is_treated_as_a_literal() {
        assert!(matches("cc[12", "cc[12"));
        assert!(!matches("cc[12", "cc1"));
    }

    #[test]
    fn empty_patterns_and_text_behave() {
        assert!(matches("", ""));
        assert!(!matches("", "x"));
        assert!(!matches("x", ""));
        assert!(matches("**", ""));
    }

    #[test]
    fn truncated_comm_names_need_a_glob_to_match() {
        // The kernel truncates comm to 15 chars, so the full name never matches.
        assert!(!matches_ci("openclaw-gateway", "openclaw-gatewa"));
        assert!(matches_ci("openclaw-gatew*", "openclaw-gatewa"));
    }
}
