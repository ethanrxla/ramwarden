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
    let cfg = Config::default();
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
        desktop: Arc::new(ramwarden_daemon::desktop::DesktopCtl::disabled()),
        voice: Default::default(),
    }
}
fn tab(id: i64) -> Tab {
    serde_json::from_value(json!({"id":id,"url":format!("https://example.org/{id}"),"title":"An old article",
        "inactiveMinutes":3000,"active":false,"pinned":false,"audible":false,"discarded":false,
        "autoDiscardable":true,"status":"complete","discardSupported":true}))
    .unwrap()
}

async fn send(st: AppState, path: &str, method: &str, body: Value) -> (u16, Value) {
    let response = ramwarden_daemon::routes::router(st)
        .oneshot(Request::builder().method(method).uri(path)
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, serde_json::from_slice(&to_bytes(response.into_body(), 1_000_000).await.unwrap()).unwrap_or(Value::Null))
}

/// A socket browser that reports one old tab and confirms closes when `confirm`.
fn browser(st: &AppState, confirm: bool) -> tokio::task::JoinHandle<Vec<Value>> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    st.hub.lock().unwrap().attach("brave", tx);
    st.hub.lock().unwrap().reg.report("brave", Transport::Socket, vec![tab(1)]);
    let other = st.clone();
    tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(raw) = rx.recv().await {
            let msg: Value = serde_json::from_str(&raw).unwrap();
            if msg["action"] == "close" && confirm {
                other.hub.lock().unwrap().deliver_close("brave", vec![1]);
            }
            let stop = msg["action"] == "close";
            seen.push(msg);
            if stop { break; }
        }
        seen
    })
}

#[tokio::test]
async fn the_warden_asks_about_a_tab_and_closes_it_when_told_to() {
    let st = state();
    let worker = browser(&st, true);
    let (status, b) = send(st.clone(), "/warden/bubble", "GET", Value::Null).await;
    assert_eq!(status, 200);
    let bubble = &b["bubble"];
    assert_eq!(bubble["kind"], "check_in", "{b}");
    assert_eq!(bubble["tab"]["id"], 1);
    assert!(bubble["text"].as_str().unwrap().contains("An old article"));
    assert_eq!(b["muted"], false);

    // Every client polls the same bubble.
    let (_, again) = send(st.clone(), "/warden/bubble", "GET", Value::Null).await;
    assert_eq!(again["bubble"]["id"], bubble["id"]);

    let (status, r) = send(st.clone(), "/warden/answer", "POST", json!({"id": bubble["id"], "choice": "close"})).await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["closed"], true);
    assert!(worker.await.unwrap().iter().any(|m| m["action"] == "close" && m["tabIds"] == json!([1])));
    // It goes in the reopen list like any other close.
    let (_, history) = send(st.clone(), "/history", "GET", Value::Null).await;
    assert_eq!(history["closed_tabs"][0]["url"], "https://example.org/1", "{history}");

    // Answering twice is refused, not repeated.
    let (status, _) = send(st.clone(), "/warden/answer", "POST", json!({"id": bubble["id"], "choice": "close"})).await;
    assert_eq!(status, 409);
}

#[tokio::test]
async fn an_unconfirmed_close_is_not_reported_as_done() {
    let st = state();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    st.hub.lock().unwrap().attach("brave", tx);
    st.hub.lock().unwrap().reg.report("brave", Transport::Socket, vec![tab(1)]);
    let (_, b) = send(st.clone(), "/warden/bubble", "GET", Value::Null).await;
    // The browser navigates the tab away before the user answers.
    let mut moved = tab(1);
    moved.url = "https://example.org/elsewhere".into();
    st.hub.lock().unwrap().reg.report("brave", Transport::Socket, vec![moved]);
    let (status, r) = send(st.clone(), "/warden/answer", "POST", json!({"id": b["bubble"]["id"], "choice": "close"})).await;
    assert_eq!(status, 200);
    assert_eq!(r["ok"], false);
    assert_eq!(r["closed"], false);
    assert!(r["reply"].as_str().unwrap().contains("couldn't"), "{r}");
}

#[tokio::test]
async fn keep_holds_the_tab_and_mute_silences_every_client() {
    let st = state();
    let _worker = browser(&st, true);
    let (_, b) = send(st.clone(), "/warden/bubble", "GET", Value::Null).await;
    let target = Target { browser: "brave".into(), id: 1, url: "https://example.org/1".into() };
    let (_, r) = send(st.clone(), "/warden/answer", "POST", json!({"id": b["bubble"]["id"], "choice": "keep"})).await;
    assert_eq!(r["ok"], true);
    assert!(st.voice.holds(&st.cfg.voice, &target, std::time::Instant::now()));
    // The reply is shown everywhere for a moment.
    let (_, reply) = send(st.clone(), "/warden/bubble", "GET", Value::Null).await;
    assert_eq!(reply["bubble"]["kind"], "reply");

    let (_, m) = send(st.clone(), "/warden/mute", "POST", json!({"muted": true, "hours": 1})).await;
    assert_eq!(m["muted"], true);
    assert!(m["muted_until"].as_u64().is_some());
    let (_, quiet) = send(st.clone(), "/warden/bubble", "GET", Value::Null).await;
    assert!(quiet["bubble"].is_null());
    assert_eq!(quiet["muted"], true);

    let (status, _) = send(st.clone(), "/warden/answer", "POST", json!({"id": "x", "choice": "maybe"})).await;
    assert_eq!(status, 422, "unknown choices are rejected");
}
