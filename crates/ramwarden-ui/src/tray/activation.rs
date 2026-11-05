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
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    nonce: u64,
    pane: String,
    #[serde(default)]
    surface: Surface,
}
fn path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("ramwarden/window-request.json")
}
fn read() -> Option<Request> {
    let bytes = std::fs::read(path()).ok()?;
    if bytes.len() > 512 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}
pub fn request(pane: Pane, surface: Surface) {
    let request = Request {
        surface,
        nonce: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64,
        pane: if pane == Pane::Ram { "ram" } else { "threat" }.into(),
    };
    let target = path();
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
}
impl Default for Reader {
    fn default() -> Self {
        Self {
            nonce: read().map(|r| r.nonce).unwrap_or(0),
            checked: Instant::now(),
        }
    }
}
impl Reader {
    pub fn next_pane(&mut self) -> Option<(Pane, Surface)> {
        if self.checked.elapsed() < Duration::from_millis(300) {
            return None;
        }
        self.checked = Instant::now();
        let request = read()?;
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

#[cfg(test)] mod tests {
 use super::*;
 #[test]fn older_dashboard_request_remains_compatible(){let r:Request=serde_json::from_str(r#"{"nonce":1,"pane":"ram"}"#).unwrap();assert!(r.surface==Surface::Dashboard);}
 #[test]fn chat_and_settings_route_independently(){for surface in [Surface::Chat,Surface::Settings] {let r=Request{nonce:2,pane:"threat".into(),surface};let restored:Request=serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();assert!(restored.surface==surface);}}
}
