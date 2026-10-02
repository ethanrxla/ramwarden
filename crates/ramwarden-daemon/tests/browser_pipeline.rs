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
use ramwarden_kernel::Root;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, RwLock};
use tower::ServiceExt;

fn state() -> AppState {
    let cfg = Config::default();
    let root = Root::at("/nonexistent");
    AppState {
        cfg: Arc::new(cfg.clone()),
        root: root.clone(),
        det: Arc::new(RwLock::new(Detector::new(root.clone()))),
        ladder: Arc::new(Mutex::new(Ladder::new(
            cfg.ladder.clone(),
            Actuator::new(root),
        ))),
        hub: Arc::new(Mutex::new(Hub::new())),
        history: Arc::new(Mutex::new(
            History::open(std::path::Path::new(":memory:")).unwrap(),
        )),
        ai: Arc::new(ramwarden_daemon::ai::provider(&cfg)),
        started: std::time::Instant::now(),
    }
}
fn tab(id: i64) -> Tab {
    serde_json::from_value(
        json!({"id":id,"url":format!("https://example.org/{id}"),"title":"article",
        "inactiveMinutes":999,"active":false,"pinned":false,"audible":false,"discarded":false,
        "autoDiscardable":true,"status":"complete","discardSupported":true}),
    )
    .unwrap()
}
async fn call(st: AppState, path: &str, method: &str, body: Value) -> Value {
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
    assert_eq!(response.status(), 200);
    serde_json::from_slice(&to_bytes(response.into_body(), 1_000_000).await.unwrap()).unwrap()
}
#[tokio::test]
async fn analysis_and_unload_flow_confirm_only_matching_browser_request_and_ids() {
    let st = state();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    st.hub.lock().unwrap().attach("browser-a", tx);
    let other = st.clone();
    let browser = tokio::spawn(async move {
        while let Some(raw) = rx.recv().await {
            let msg: Value = serde_json::from_str(&raw).unwrap();
            let mut hub = other.hub.lock().unwrap();
            match msg["action"].as_str().unwrap() {
                "get_tabs" => hub.deliver_tabs("browser-a", vec![tab(1), tab(2)]),
                "discard" => {
                    let request = msg["requestId"].as_str().unwrap();
                    hub.deliver_discard("wrong-browser", request, vec![2]);
                    hub.deliver_discard("browser-a", request, vec![1, 9999]);
                }
                _ => panic!("unexpected browser command"),
            }
        }
    });
    let analysis = call(st.clone(), "/browser/analyze", "POST", json!({})).await;
    assert_eq!(analysis["tabs"].as_array().unwrap().len(), 2);
    assert_eq!(analysis["candidates"], 2);
    let targets = vec![
        Target {
            browser: "browser-a".into(),
            id: 1,
            url: tab(1).url,
        },
        Target {
            browser: "browser-a".into(),
            id: 2,
            url: tab(2).url,
        },
    ];
    let result = call(
        st.clone(),
        "/browser/discard",
        "POST",
        json!({"targets":targets}),
    )
    .await;
    assert_eq!(result["confirmed"].as_array().unwrap().len(), 1);
    assert_eq!(result["confirmed"][0]["id"], 1);
    assert_eq!(result["refused"][0]["id"], 2);
    let repeat = st.discard_tabs(&targets).await;
    assert!(repeat.confirmed.is_empty());
    assert_eq!(repeat.refused.len(), 2);
    browser.abort();
}
#[tokio::test]
async fn polling_delivery_is_queued_not_claimed_as_savings() {
    let st = state();
    st.hub
        .lock()
        .unwrap()
        .reg
        .report("ff", Transport::Poll, vec![tab(1)]);
    let targets = vec![Target {
        browser: "ff".into(),
        id: 1,
        url: tab(1).url,
    }];
    let result = st.discard_tabs(&targets).await;
    assert!(result.confirmed.is_empty());
    assert_eq!(result.queued, targets);
    let commands = st.hub.lock().unwrap().reg.take_queued("ff");
    assert_eq!(commands[0].action, "discard");
    assert_eq!(commands[0].tabs, targets);
    assert!(st.discard_tabs(&targets).await.queued.is_empty());
}
#[tokio::test]
async fn legacy_extension_is_visible_but_cannot_be_unloaded() {
    let st = state();
    st.hub.lock().unwrap().reg.report(
        "old",
        Transport::Poll,
        vec![
            serde_json::from_value(
                json!({"id":1,"url":"https://example.org","inactiveMinutes":999}),
            )
            .unwrap(),
        ],
    );
    let result = call(st, "/browser/tabs", "GET", json!({})).await;
    assert_eq!(result["candidates"], 0);
    assert_eq!(result["tabs"][0]["status"], "update needed");
}

#[tokio::test(start_paused = true)]
async fn a_silent_browser_times_out_without_claiming_reclamation() {
    let st = state();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    st.hub.lock().unwrap().attach("silent", tx);
    st.hub.lock().unwrap().deliver_tabs("silent", vec![tab(1)]);
    let target = Target {
        browser: "silent".into(),
        id: 1,
        url: tab(1).url,
    };
    let result = st.discard_tabs(std::slice::from_ref(&target)).await;
    assert!(result.confirmed.is_empty());
    assert_eq!(result.refused, vec![target]);
}

#[tokio::test]
async fn manual_close_works_with_legacy_extension_and_refuses_navigation_or_ambiguous_ids() {
    let st = state();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    st.hub.lock().unwrap().attach("legacy", tx);
    let other = st.clone();
    let worker = tokio::spawn(async move {
        while let Some(raw) = rx.recv().await {
            let msg: Value = serde_json::from_str(&raw).unwrap();
            let mut hub = other.hub.lock().unwrap();
            match msg["action"].as_str().unwrap() {
                "get_tabs" => hub.deliver_tabs(
                    "legacy",
                    vec![
                        serde_json::from_value(
                            json!({"id":7,"url":"https://example.org/article","inactiveMinutes":0}),
                        )
                        .unwrap(),
                    ],
                ),
                "close" => {
                    assert_eq!(msg["tabIds"], json!([7]));
                    hub.deliver_close("legacy", vec![7]);
                }
                _ => panic!("unexpected command"),
            }
        }
    });
    let analysis = call(st.clone(), "/browser/analyze", "POST", json!({})).await;
    assert_eq!(analysis["tabs"][0]["closeable"], true);
    assert_eq!(analysis["tabs"][0]["eligible"], false);
    let target = Target {
        browser: "legacy".into(),
        id: 7,
        url: "https://example.org/article".into(),
    };
    let mut wrong = target.clone();
    wrong.url.push_str("/changed");
    assert_eq!(st.close_selected_tabs(&[wrong]).await.refused.len(), 1);
    st.hub.lock().unwrap().reg.report(
        "collision",
        Transport::Poll,
        vec![Tab {
            id: 7,
            url: target.url.clone(),
            ..Default::default()
        }],
    );
    assert_eq!(
        st.close_selected_tabs(std::slice::from_ref(&target))
            .await
            .refused
            .len(),
        1
    );
    st.hub
        .lock()
        .unwrap()
        .reg
        .report("collision", Transport::Poll, vec![]);
    let result = call(
        st.clone(),
        "/browser/close",
        "POST",
        json!({"targets":[target]}),
    )
    .await;
    assert_eq!(result["confirmed"].as_array().unwrap().len(), 1);
    assert_eq!(result["confirmed"][0]["browser"], "legacy");
    assert!(st.hub.lock().unwrap().reg.tabs_for("legacy").is_empty());
    worker.abort();
}
