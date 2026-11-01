//! `/desktop` over the real router, against an in-memory window manager.
//!
//! The contract under test is the one a chat client relies on: asking for a
//! change returns a plan and moves nothing; applying that plan moves exactly
//! what it listed, once.

use std::sync::{Arc, Mutex, RwLock};

use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use ramwarden_core::{actuator::Actuator, config::Config, detector::Detector, history::History, ladder::Ladder};
use ramwarden_daemon::{
    desktop::DesktopCtl,
    hub::{AppState, Hub},
};
use ramwarden_platform::{Result, WindowManager, WmCommand, WmKind, WmState, WmWindow, WmWorkspace, fake};
use ramwarden_wm::ConfigBackups;
use serde_json::{Value, json};
use tower::ServiceExt;

struct MemWm(Arc<Mutex<WmState>>);

impl WindowManager for MemWm {
    fn kind(&self) -> WmKind {
        WmKind::AeroSpace
    }
    fn state(&mut self) -> Result<WmState> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn run(&mut self, cmd: &WmCommand) -> Result<()> {
        if let WmCommand::MoveToWorkspace { window, workspace } = cmd {
            let mut s = self.0.lock().unwrap();
            if let Some(w) = s.windows.iter_mut().find(|w| w.id == *window) {
                w.workspace = workspace.clone();
            }
        }
        Ok(())
    }
    fn config_path(&self) -> Option<std::path::PathBuf> {
        None
    }
    fn check_config(&self, _: &str) -> Result<()> {
        Ok(())
    }
    fn reload_config(&mut self) -> Result<()> {
        Ok(())
    }
}

fn state(desktop: DesktopCtl) -> AppState {
    let cfg = Config::default();
    let platform = Arc::new(fake::platform(Default::default()));
    AppState {
        cfg: Arc::new(cfg.clone()),
        det: Arc::new(RwLock::new(Detector::new(platform.processes()))),
        ladder: Arc::new(Mutex::new(Ladder::new(cfg.ladder.clone(), Actuator::for_platform(&platform)))),
        platform,
        hub: Arc::new(Mutex::new(Hub::new())),
        history: Arc::new(Mutex::new(History::open(std::path::Path::new(":memory:")).unwrap())),
        ai: Arc::new(ramwarden_daemon::ai::provider(&cfg)),
        models: Arc::new(Default::default()),
        predictor: Arc::new(ramwarden_daemon::predict::LivePredictor::new(
            std::env::temp_dir().join("ramwarden-test-no-predictor.safetensors"),
        )),
        started: std::time::Instant::now(),
        hold_until: Default::default(),
        desktop: Arc::new(desktop),
    }
}

async fn call(st: AppState, method: &str, path: &str, body: Value) -> (u16, Value) {
    let response = ramwarden_daemon::routes::router(st)
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("Content-Type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn desktop() -> (Arc<Mutex<WmState>>, DesktopCtl, tempfile::TempDir) {
    let wm = Arc::new(Mutex::new(WmState {
        kind: WmKind::AeroSpace,
        windows: vec![WmWindow {
            id: 4242,
            app: "Slack".into(),
            title: "general".into(),
            workspace: "1".into(),
            pid: Some(1),
            focused: true,
        }],
        workspaces: vec![WmWorkspace { name: "1".into(), monitor: None, focused: true }],
        monitors: vec![],
    }));
    let tmp = tempfile::tempdir().unwrap();
    let w = wm.clone();
    let ctl = DesktopCtl::with(
        move || Some(Box::new(MemWm(w.clone())) as Box<dyn WindowManager>),
        ConfigBackups::new(tmp.path()),
    );
    (wm, ctl, tmp)
}

#[tokio::test]
async fn a_plan_moves_nothing_until_it_is_applied_and_applies_once() {
    let (wm, ctl, _tmp) = desktop();
    let st = state(ctl);

    let (code, body) = call(
        st.clone(),
        "POST",
        "/desktop/plan",
        json!({"commands": [{"op": "move_to_workspace", "window": 4242, "workspace": "3"}]}),
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(wm.lock().unwrap().windows[0].workspace, "1", "planning must not move anything");
    let id = body["plan"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["plan"]["steps"][0]["describe"], "Move Slack \u{2014} general to workspace 3");

    let (_, status) = call(st.clone(), "GET", "/desktop", json!(null)).await;
    assert_eq!(status["pending"].as_array().unwrap().len(), 1);

    let (code, applied) = call(st.clone(), "POST", &format!("/desktop/apply/{id}"), json!(null)).await;
    assert_eq!(code, 200);
    assert_eq!(applied["applied"]["steps"][0]["status"], "done");
    assert_eq!(applied["applied"]["changed"], true);
    assert_eq!(wm.lock().unwrap().windows[0].workspace, "3");
    assert!(st.on_hold(), "freezing is held while windows move");

    let (code, _) = call(st, "POST", &format!("/desktop/apply/{id}"), json!(null)).await;
    assert_eq!(code, 404, "a plan is single-use");
}

#[tokio::test]
async fn turned_off_or_absent_is_a_normal_answer_not_an_error() {
    let st = state(DesktopCtl::disabled());
    let (code, body) = call(st.clone(), "GET", "/desktop", json!(null)).await;
    assert_eq!(code, 200);
    assert_eq!(body["available"], false);

    let (code, body) = call(st, "POST", "/desktop/plan", json!({"commands": []})).await;
    assert_eq!(code, 200);
    assert_eq!(body["available"], false);
}

#[tokio::test]
async fn asking_without_any_model_returns_notes_and_no_plan() {
    let (_wm, ctl, _tmp) = desktop();
    let mut st = state(ctl);
    // No Ollama, cloud off: nothing to ask.
    let mut provider = ramwarden_daemon::ai::provider(&Config::default());
    provider.ollama = None;
    st.ai = Arc::new(provider);
    let (code, body) = call(st, "POST", "/desktop/ask", json!({"request": "put slack on 3"})).await;
    assert_eq!(code, 200);
    assert!(body["plan"].is_null());
    assert!(!body["notes"].as_array().unwrap().is_empty());
}
