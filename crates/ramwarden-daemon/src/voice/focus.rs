//! A local focus session only adds protection; it never permits another close.
use std::{collections::HashSet, path::PathBuf, sync::Mutex};
use serde::{Deserialize, Serialize};

const SESSION: u64 = 8 * 3600;
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Session { goal: String, until: u64, asked_at: u64 }
pub struct Focus { state: Mutex<Session>, path: Option<PathBuf> }
impl Focus {
    pub fn new(path: Option<PathBuf>) -> Self {
        let state = path.as_ref().and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { state: Mutex::new(state), path }
    }
    fn save(&self, state: &Session) {
        use std::io::Write;
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
            if let Ok(mut file) = options.open(path) { let _ = file.write_all(&serde_json::to_vec(state).unwrap_or_default()); }
        }
    }
    pub fn set(&self, goal: &str, now: u64) {
        let mut state = self.state.lock().unwrap();
        state.goal = goal.trim().to_string();
        state.until = if state.goal.is_empty() { 0 } else { now.saturating_add(SESSION) };
        state.asked_at = now;
        self.save(&state);
    }
    pub fn goal(&self, now: u64) -> String {
        let state = self.state.lock().unwrap();
        if now < state.until { state.goal.clone() } else { String::new() }
    }
    pub fn ask(&self, now: u64) -> bool {
        let mut state = self.state.lock().unwrap();
        if now < state.until || now.saturating_sub(state.asked_at) < SESSION { return false; }
        state.asked_at = now;
        self.save(&state);
        true
    }
    pub fn matches(&self, title: &str, url: &str, now: u64) -> bool {
        let goal = self.goal(now);
        let terms = words(&goal);
        let page = words(&format!("{title} {url}"));
        terms.iter().any(|term| page.contains(term))
    }
}
fn words(text: &str) -> HashSet<String> {
    const STOP: &[&str] = &["the","and","for","with","this","that","working","work","today","project","about","need","want","are","you","what","building","doing","https","http","www","com"];
    text.to_lowercase().split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.len() >= 3 && !STOP.contains(word)).map(String::from).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn focus_protects_matching_topics_expires_and_can_be_cleared() {
        let f = Focus::new(None);
        f.set("working on MongoDB support", 100_000);
        assert!(f.matches("MongoDB Atlas", "https://cloud.mongodb.com", 100_001));
        assert!(!f.matches("A working project", "https://example.com", 100_001));
        assert!(!f.matches("MongoDB Atlas", "https://cloud.mongodb.com", 100_000 + SESSION));
        f.set("Rust", 200_000); f.set("", 200_001);
        assert!(!f.matches("Rust book", "https://rust-lang.org", 200_002));
    }
    #[test]
    fn goal_and_prompt_throttle_survive_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("focus.json");
        let f = Focus::new(Some(path.clone()));
        assert!(f.ask(100_000));
        let f = Focus::new(Some(path.clone()));
        assert!(!f.ask(100_001));
        f.set("Rust", 100_002);
        let f = Focus::new(Some(path));
        assert_eq!(f.goal(100_003), "Rust");
    }
}
