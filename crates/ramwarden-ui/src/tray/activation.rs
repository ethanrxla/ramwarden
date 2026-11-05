//! A UI-only pane request. This never carries commands or execution authority.
use super::Pane;
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum Surface {
    #[default]
    Dashboard,
    Chat,
    Settings,
    Thought,
    ToggleChat,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    nonce: u64,
    pane: String,
    #[serde(default)]
    surface: Surface,
}
fn path(companion: Option<Pane>) -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(match companion {
            None => "ramwarden/window-request.json",
            Some(Pane::Ram) => "ramwarden/ram-chat-request.json",
            Some(Pane::Threat) => "ramwarden/threat-chat-request.json",
        })
}
fn read(companion: Option<Pane>) -> Option<Request> {
    let bytes = std::fs::read(path(companion)).ok()?;
    if bytes.len() > 512 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}
pub fn request(pane: Pane, surface: Surface, companion: bool) {
    let request = Request {
        surface,
        nonce: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64,
        pane: if pane == Pane::Ram { "ram" } else { "threat" }.into(),
    };
    let target = path(if companion { Some(pane) } else { None });
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(&request) {
        let pending = target.with_extension("tmp");
        if std::fs::write(&pending, bytes).is_ok() {
            let _ = std::fs::rename(pending, target);
        }
    }
}
pub struct Reader {
    nonce: u64,
    checked: Instant,
    companion: Option<Pane>,
}
impl Default for Reader {
    fn default() -> Self {
        Self {
            companion: None,
            nonce: read(None).map(|r| r.nonce).unwrap_or(0),
            checked: Instant::now(),
        }
    }
}
impl Reader {
    pub fn for_companion(pane: Pane) -> Self {
        Self {
            nonce: read(Some(pane)).map(|r| r.nonce).unwrap_or(0),
            checked: Instant::now(),
            companion: Some(pane),
        }
    }
    pub fn next_pane(&mut self) -> Option<(Pane, Surface)> {
        if self.checked.elapsed() < Duration::from_millis(300) {
            return None;
        }
        self.checked = Instant::now();
        let request = read(self.companion)?;
        if request.nonce <= self.nonce {
            return None;
        }
        self.nonce = request.nonce;
        match request.pane.as_str() {
            "ram" => Some((Pane::Ram, request.surface)),
            "threat" => Some((Pane::Threat, request.surface)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dashboard_and_each_pet_have_distinct_channels() {
        assert_ne!(path(None), path(Some(Pane::Ram)));
        assert_ne!(path(Some(Pane::Ram)), path(Some(Pane::Threat)));
    }
    #[test]
    fn older_dashboard_request_remains_compatible() {
        let r: Request = serde_json::from_str(r#"{"nonce":1,"pane":"ram"}"#).unwrap();
        assert!(r.surface == Surface::Dashboard);
    }
    #[test]
    fn chat_and_settings_route_independently() {
        for surface in [Surface::Chat, Surface::Settings] {
            let r = Request {
                nonce: 2,
                pane: "threat".into(),
                surface,
            };
            let restored: Request =
                serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();
            assert!(restored.surface == surface);
        }
    }
}
