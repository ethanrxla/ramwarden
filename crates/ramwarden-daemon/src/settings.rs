//! Narrow, live settings: persist before publishing, without rewriting secrets or
//! unrelated TOML. The daemon's normal Host/Origin guard protects these routes.
use std::{io::Write, path::{Path, PathBuf}, sync::Mutex};
use axum::{extract::State, http::StatusCode, routing::get, Json};
use serde::{Deserialize, Serialize};
use ramwarden_core::config::{Config, Tabs};
use crate::hub::AppState;

#[derive(Default)]
pub struct TabBudget(Mutex<Option<usize>>);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TabSettings {
    pub keep_tabs: usize,
}

impl TabBudget {
    pub fn effective(&self, cfg: &Tabs) -> Tabs {
        let mut tabs = cfg.clone();
        if let Some(keep) = *self.0.lock().unwrap() { tabs.keep_tabs = keep; }
        tabs
    }

    fn save(&self, cfg: &Config, keep: usize) -> Result<(), SettingsError> {
        if !(1..=100).contains(&keep) { return Err(SettingsError::Invalid); }
        // Serializes concurrent settings updates, including the disk commit. A
        // failed write never changes the running policy.
        let mut current = self.0.lock().unwrap();
        let path = cfg.source.clone().unwrap_or_else(default_path);
        persist(&path, keep).map_err(|_| SettingsError::Persistence)?;
        *current = Some(keep);
        Ok(())
    }
}

fn default_path() -> PathBuf {
    std::env::var_os("RAMWARDEN_CONFIG").map(PathBuf::from)
        .unwrap_or_else(|| ramwarden_core::config::config_dir().join("ramwarden/config.toml"))
}

#[derive(Debug)]
enum SettingsError { Invalid, Persistence }

fn persist(path: &Path, keep: usize) -> Result<(), Box<dyn std::error::Error>> {
    // Follow an existing config symlink rather than replacing it.
    let path = if path.exists() { path.canonicalize()? } else { path.to_path_buf() };
    let old = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc = old.parse::<toml_edit::Document>()?;
    if !doc.contains_key("tabs") { doc["tabs"] = toml_edit::Item::Table(toml_edit::Table::new()); }
    let table = doc["tabs"].as_table_like_mut().ok_or("tabs is not a table")?;
    if let Some(item) = table.get_mut("keep_tabs") {
        let value = item.as_value_mut().ok_or("keep_tabs is not a value")?;
        let decor = value.decor().clone();
        *value = toml_edit::Value::from(keep as i64);
        *value.decor_mut() = decor;
    } else { table.insert("keep_tabs", toml_edit::value(keep as i64)); }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut pending = tempfile::NamedTempFile::new_in(parent)?;
    if let Ok(meta) = std::fs::metadata(&path) { pending.as_file().set_permissions(meta.permissions())?; }
    pending.write_all(doc.to_string().as_bytes())?;
    pending.as_file().sync_all()?;
    pending.persist(path)?;
    Ok(())
}

pub fn routes() -> axum::Router<AppState> {
    axum::Router::new().route("/settings/tabs", get(read).post(update))
}
async fn read(State(st): State<AppState>) -> Json<TabSettings> {
    Json(TabSettings { keep_tabs: st.tabs_config().keep_tabs })
}
async fn update(State(st): State<AppState>, Json(input): Json<TabSettings>) -> Result<Json<TabSettings>, (StatusCode, &'static str)> {
    if !(1..=100).contains(&input.keep_tabs) { return Err((StatusCode::BAD_REQUEST, "keep_tabs must be between 1 and 100")); }
    let settings = st.hub.lock().unwrap().tab_budget.clone();
    let result = tokio::task::spawn_blocking(move || settings.save(&st.cfg, input.keep_tabs)).await;
    match result {
        Ok(Ok(())) => Ok(Json(input)),
        Ok(Err(SettingsError::Invalid)) => Err((StatusCode::BAD_REQUEST, "keep_tabs must be between 1 and 100")),
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, "Could not save tab settings; running settings unchanged")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_budget_changes_live_close_policy_and_survives_reload_without_losing_toml() {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("config.toml");
        let original = "# user configuration\n[thresholds]\nram_percent = 79.0 # keep\n[tabs]\nkeep_tabs = 6 # recent tabs\nbudget_idle_minutes = 60.0\n[unrelated]\nprivate_value = 'preserve-me'\n";
        std::fs::write(&path, original).unwrap();
        let cfg = Config {source: Some(path.clone()), ..Config::default()};
        let settings = TabBudget::default();
        let now = ramwarden_core::history::unix_now() as f64 * 1000.0;
        let tabs: Vec<_> = (0..6).map(|id| crate::tabs::Tab {
            id, url: format!("https://example{id}.org/"), inactive_minutes: 2000 + id,
            active: Some(false), pinned: Some(false), audible: Some(false),
            discarded: Some(false), discard_supported: Some(true),
            status: Some("complete".into()), auto_discardable: Some(true), activity_version: 1,
            last_active_ms: now - (2000 + id) as f64 * 60000.0, ..Default::default()
        }).collect();
        let rows = crate::browser::rank_at("browser", &tabs, 30, now);
        assert!(crate::browser::close_candidates(&rows, &settings.effective(&cfg.tabs)).is_empty());
        settings.save(&cfg, 2).unwrap();
        let live = settings.effective(&cfg.tabs);
        let selected = crate::browser::close_candidates(&rows, &live);
        assert_eq!(selected.len(), 4);
        assert!(selected.iter().all(|t| t.id >= 2));
        let mut two_browsers = rows.clone();
        two_browsers.extend(rows.iter().cloned().map(|mut r| { r.target.browser = "second".into(); r }));
        let selected = crate::browser::close_candidates(&two_browsers, &live);
        assert_eq!(selected.len(), 8, "each browser keeps its own newest two");
        assert!(selected.iter().all(|t| t.id >= 2));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, original.replace("keep_tabs = 6", "keep_tabs = 2"));
        assert!(text.contains("ram_percent = 79.0 # keep"));
        assert!(text.contains("private_value = 'preserve-me'"));
        assert_eq!(ramwarden_core::config::parse(&text).unwrap().tabs.keep_tabs, 2);
    }
    #[test]
    fn failed_or_invalid_save_keeps_both_runtime_and_existing_file() {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("config.toml");
        std::fs::write(&path, "not = valid = toml").unwrap();
        let cfg = Config {source: Some(path.clone()), ..Config::default()};
        let settings = TabBudget::default();
        assert!(matches!(settings.save(&cfg, 0), Err(SettingsError::Invalid)));
        assert!(matches!(settings.save(&cfg, 101), Err(SettingsError::Invalid)));
        assert!(matches!(settings.save(&cfg, 2), Err(SettingsError::Persistence)));
        assert_eq!(settings.effective(&cfg.tabs).keep_tabs, 6);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "not = valid = toml");
    }
}
