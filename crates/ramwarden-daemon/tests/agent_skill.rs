//! Keeps the agent skill (`agent/skills/ramwarden`) honest about this daemon.
//!
//! Agents follow the skill literally and cannot see the code, so a route the
//! skill names but the daemon does not serve fails silently on the user's
//! machine: the agent gets a 404 and tells the user RamWarden is broken. These
//! tests read the skill and the router source and fail CI instead. They read
//! files only; nothing here starts the daemon or touches a process.

use std::fs;
use std::path::{Path, PathBuf};

/// Routes the skill may name before this branch serves them, because another
/// open change adds them. The skill marks them as optional (an older daemon
/// answers 404). Remove an entry once its route is on `main`.
const PENDING: &[&str] = &[
    "/wardens",    // the warden registry (ThreatWarden)
    "/wardens/{}", // one warden and its report
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn skill_dir() -> PathBuf {
    repo().join("agent/skills/ramwarden")
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// `(method, pattern)` for every `.route("…", get(…).post(…))` in the daemon,
/// with path parameters reduced to `{}`.
fn served_routes() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let src = repo().join("crates/ramwarden-daemon/src");
    for entry in fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = read(&path);
        let mut rest = text.as_str();
        while let Some(i) = rest.find(".route(\"") {
            rest = &rest[i + 8..];
            let Some(end) = rest.find('"') else { break };
            let pattern = normalise(&rest[..end]);
            // The handlers follow up to the closing paren of `.route(`.
            let call_end = rest.find("))").map(|j| j + 2).unwrap_or(rest.len());
            let call = &rest[end..call_end];
            for (needle, method) in [("get(", "GET"), ("post(", "POST"), ("delete(", "DELETE"), ("put(", "PUT")] {
                if call.contains(needle) {
                    out.push((method.to_string(), pattern.clone()));
                }
            }
            rest = &rest[end..];
        }
    }
    out
}

fn normalise(path: &str) -> String {
    path.split('/')
        .map(|seg| if seg.starts_with('{') { "{}" } else { seg })
        .collect::<Vec<_>>()
        .join("/")
}

/// Does a concrete or templated path from the skill match a router pattern?
fn matches(pattern: &str, path: &str) -> bool {
    let p: Vec<_> = pattern.split('/').collect();
    let q: Vec<_> = path.split('/').collect();
    p.len() == q.len() && p.iter().zip(&q).all(|(a, b)| *a == "{}" || a == b)
}

/// Every `` `METHOD /path` `` the skill tells an agent to call on the daemon.
/// Hosted API routes (`/v1/…`) live on the account server, not here.
fn skill_calls() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for file in ["SKILL.md", "references/api.md", "references/install.md"] {
        let text = read(&skill_dir().join(file));
        for chunk in text.split('`').skip(1).step_by(2) {
            let mut words = chunk.split_whitespace();
            let (Some(method), Some(path), None) = (words.next(), words.next(), words.next()) else {
                continue;
            };
            if !["GET", "POST", "DELETE", "PUT"].contains(&method) || !path.starts_with('/') {
                continue;
            }
            if path.starts_with("/v1/") {
                continue;
            }
            let path = path.split('?').next().unwrap();
            out.push((method.to_string(), normalise(path)));
        }
    }
    out
}

#[test]
fn every_route_the_skill_names_is_served() {
    let served = served_routes();
    assert!(served.len() > 20, "router parse found only {served:?}");
    let calls = skill_calls();
    assert!(calls.len() > 15, "skill parse found only {calls:?}");

    let missing: Vec<_> = calls
        .iter()
        .filter(|(m, path)| !served.iter().any(|(sm, pat)| sm == m && matches(pat, path)))
        .filter(|(_, path)| !PENDING.iter().any(|pat| matches(pat, path)))
        .collect();
    assert!(missing.is_empty(), "the skill names routes the daemon does not serve: {missing:?}");
}

#[test]
fn skill_frontmatter_is_valid() {
    let text = read(&skill_dir().join("SKILL.md"));
    let front = text
        .strip_prefix("---\n")
        .and_then(|t| t.split_once("\n---\n"))
        .map(|(f, _)| f)
        .expect("SKILL.md starts with a --- frontmatter block");
    let field = |key: &str| {
        front
            .lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(':')))
            .map(str::trim)
            .unwrap_or_else(|| panic!("frontmatter has no {key}"))
    };

    // The Agent Skills format: the name is the folder's name, lowercase
    // letters, digits and hyphens; the description is what agents match
    // requests against, at most 1024 characters, with no angle brackets.
    let name = field("name");
    assert_eq!(name, "ramwarden");
    assert!(name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    let description = field("description");
    assert!(!description.is_empty() && description.len() <= 1024);
    assert!(!description.contains('<') && !description.contains('>'));
}

#[test]
fn skill_links_resolve() {
    for file in ["SKILL.md", "references/buying.md"] {
        let path = skill_dir().join(file);
        let text = read(&path);
        for link in text.split("](").skip(1).filter_map(|r| r.split(')').next()) {
            if link.starts_with("http") || link.starts_with('#') {
                continue;
            }
            let target = path.parent().unwrap().join(link);
            assert!(target.exists(), "{file} links to missing {link}");
        }
    }
}

/// The plugin's version moves with the app's, so an agent can tell which
/// daemon the skill was written against.
#[test]
fn plugin_version_matches_the_workspace() {
    let manifest: serde_json::Value =
        serde_json::from_str(&read(&repo().join("agent/.claude-plugin/plugin.json"))).unwrap();
    assert_eq!(manifest["name"], "ramwarden");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));

    let market: serde_json::Value =
        serde_json::from_str(&read(&repo().join(".claude-plugin/marketplace.json"))).unwrap();
    let source = market["plugins"][0]["source"].as_str().unwrap();
    assert!(repo().join(source).join(".claude-plugin/plugin.json").exists());
}
