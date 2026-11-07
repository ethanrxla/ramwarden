use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use ramwarden_core::{
    actuator::Actuator, config::Config, detector::Detector, history::History, ladder::Ladder,
};
use ramwarden_daemon::{
    browser::Target,
    hub::{AppState, Hub},
    tabs::{Tab, Transport},
};
use ramwarden_platform::fake;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, RwLock};
use tower::ServiceExt;

fn state() -> AppState {
    let mut cfg = Config::default();
    cfg.tabs.close = "live".into();
    let platform = Arc::new(fake::platform(Default::default()));
    AppState {
        cfg: Arc::new(cfg.clone()),
        det: Arc::new(RwLock::new(Detector::new(platform.processes()))),
        ladder: Arc::new(Mutex::new(Ladder::new(
            cfg.ladder.clone(),
            Actuator::for_platform(&platform),
        ))),
        platform,
        hub: Arc::new(Mutex::new(Hub::new())),
        history: Arc::new(Mutex::new(
            History::open(std::path::Path::new(":memory:")).unwrap(),
        )),
        ai: Arc::new(ramwarden_daemon::ai::provider(&cfg)),
        models: Arc::new(Default::default()),
        predictor: Arc::new(ramwarden_daemon::predict::LivePredictor::new(
            std::env::temp_dir().join("ramwarden-test-no-predictor.safetensors"),
        )),
        started: std::time::Instant::now(),
        hold_until: Default::default(),
        voice: Default::default(),
        desktop: Arc::new(ramwarden_daemon::desktop::DesktopCtl::disabled()),
    }
}

fn tab(id: i64) -> Tab {
    serde_json::from_value(json!({"id":id,"url":format!("https://example.org/{id}"),"title":"review test",
        "inactiveMinutes":180,"lastActiveMs":ramwarden_core::history::unix_now() as f64*1000.0-180.0*60000.0,
        "activityVersion":1,"active":false,"pinned":false,"audible":false,"discarded":false,
        "autoDiscardable":true,"status":"complete","discardSupported":true})).unwrap()
}
async fn request(st: AppState, path: &str, body: Option<Value>) -> (u16, Value) {
    let builder = Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(path)
        .header("Content-Type", "application/json");
    let response = ramwarden_daemon::routes::router(st)
        .oneshot(
            builder
                .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1000000).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
#[tokio::test]
async fn keep_is_selected_only_single_use_and_later_never_closes() {
    let st = state();
    st.hub
        .lock()
        .unwrap()
        .reg
        .report("b", Transport::Poll, vec![tab(1), tab(2)]);
    let (_, v) = request(st.clone(), "/browser/review", None).await;
    let p = &v["proposal"];
    let target = Target {
        browser: "b".into(),
        id: 1,
        url: tab(1).url,
    };
    let invalid = json!({"token":p["token"],"action":"keep","tabs":[{"browser":"b","id":99,"url":"https://example.org/99"}]});
    assert_eq!(
        request(st.clone(), "/browser/review/respond", Some(invalid))
            .await
            .0,
        400
    );
    let body = json!({"token":p["token"],"action":"keep","tabs":[target]});
    assert_eq!(
        request(st.clone(), "/browser/review/respond", Some(body.clone()))
            .await
            .0,
        200
    );
    assert_eq!(
        request(st.clone(), "/browser/review/respond", Some(body))
            .await
            .0,
        409
    );
    let rows = ramwarden_daemon::tabreview::snapshot(&st);
    assert!(
        !rows
            .iter()
            .find(|r| r.target.id == 1)
            .unwrap()
            .auto_closeable
    );
    assert!(
        rows.iter()
            .find(|r| r.target.id == 2)
            .unwrap()
            .auto_closeable
    );
    let fresh = state();
    fresh
        .hub
        .lock()
        .unwrap()
        .reg
        .report("b", Transport::Poll, vec![tab(1)]);
    let (_, v) = request(fresh.clone(), "/browser/review", None).await;
    assert_eq!(
        request(
            fresh.clone(),
            "/browser/review/respond",
            Some(json!({"token":v["proposal"]["token"],"action":"later"}))
        )
        .await
        .0,
        200
    );
    assert_eq!(fresh.hub.lock().unwrap().reg.tabs_for("b").len(), 1);
}
#[tokio::test]
async fn close_rechecks_activity_and_sends_only_selected_identity() {
    for became_active in [false, true] {
        let st = state();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        st.hub.lock().unwrap().attach("b", tx);
        st.hub
            .lock()
            .unwrap()
            .deliver_tabs("b", vec![tab(1), tab(2)]);
        let (_, v) = request(st.clone(), "/browser/review", None).await;
        let other = st.clone();
        let worker = tokio::spawn(async move {
            while let Some(raw) = rx.recv().await {
                let msg: Value = serde_json::from_str(&raw).unwrap();
                let mut hub = other.hub.lock().unwrap();
                match msg["action"].as_str().unwrap() {
                    "get_tabs" => {
                        let mut t = tab(1);
                        t.active = Some(became_active);
                        hub.deliver_tabs("b", vec![t, tab(2)]);
                    }
                    "close_safe" => {
                        assert!(!became_active);
                        assert_eq!(msg["tabs"].as_array().unwrap().len(), 1);
                        assert_eq!(msg["tabs"][0]["id"], 1);
                        hub.deliver_discard("b", msg["requestId"].as_str().unwrap(), vec![1]);
                    }
                    _ => panic!("unexpected command"),
                }
            }
        });
        let (status,result)=request(st.clone(),"/browser/review/respond",Some(json!({"token":v["proposal"]["token"],"action":"close","tabs":[{"browser":"b","id":1,"url":tab(1).url}]}))).await;
        assert_eq!(status, 200);
        assert_eq!(
            result["confirmed"].as_array().unwrap().len(),
            usize::from(!became_active)
        );
        assert_eq!(
            result["refused"].as_array().unwrap().len(),
            usize::from(became_active)
        );
        worker.abort();
    }
}
