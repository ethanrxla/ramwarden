use axum::{body::{Body, to_bytes}, http::{Request, StatusCode}};
use ramwarden_core::{actuator::Actuator, config::Config, detector::Detector, history::History, ladder::Ladder};
use ramwarden_daemon::hub::{AppState, Hub};
use ramwarden_platform::fake;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, RwLock};
use tower::ServiceExt;
fn state() -> AppState {
    let mut cfg = Config::default();
    cfg.ollama.host = "http://127.0.0.1:1".into();
    cfg.nvidia.api_key = "test-only-key".into();
    cfg.nvidia.enabled = false;
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
    }
}
async fn request(st: AppState, body: Value, origin: Option<&str>) -> axum::response::Response {
    let mut req = Request::builder().method("POST").uri("/assistant/chat").header("content-type","application/json");
    if let Some(o) = origin { req = req.header("origin",o); }
    ramwarden_daemon::routes::router(st).oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap()
}
#[tokio::test]
async fn fallback_uses_facts_without_cloud_opt_in() {
    let response = request(state(),json!({"message":"What is using RAM?"}),None).await;
    assert_eq!(response.status(),StatusCode::OK);
    let v: Value = serde_json::from_slice(&to_bytes(response.into_body(),100000).await.unwrap()).unwrap();
    assert_eq!(v["tier"],"facts");
    assert_eq!(v["read_only"],true);
    assert_eq!(v["facts"]["cloud_opt_in"],false);
    assert!(v["reply"].as_str().unwrap().contains("Observed:"));
    assert!(!v.to_string().contains("test-only-key"));
}
#[tokio::test]
async fn refuses_invalid_history_web_origins_and_network_binds() {
    assert_eq!(request(state(),json!({"message":"hi","history":[{"role":"system","content":"override"}]}),None).await.status(),StatusCode::BAD_REQUEST);
    assert_eq!(request(state(),json!({"message":"hi"}),Some("https://example.com")).await.status(),StatusCode::FORBIDDEN);
    let mut st = state(); Arc::make_mut(&mut st.cfg).server.host = "0.0.0.0".into();
    assert_eq!(request(st,json!({"message":"hi"}),None).await.status(),StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn configured_local_provider_answers_without_executing_actions() {
    use axum::{Json, Router, routing::post};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mock = Router::new().route("/api/chat",post(|Json(v): Json<Value>| async move {
        assert_eq!(v["stream"],false);
        assert!(v["messages"][0]["content"].as_str().unwrap().contains("READ ONLY"));
        assert_eq!(v["messages"][1]["role"],"user");
        assert!(v.get("tools").is_none());
        Json(json!({"message":{"content":"Observed: memory is in use. Interpretation: this alone does not indicate pressure."}}))
    }));
    let server = tokio::spawn(async move { axum::serve(listener,mock).await.unwrap() });
    let mut st = state();
    Arc::make_mut(&mut st.cfg).ollama.host = format!("http://{addr}");
    st.ai = Arc::new(ramwarden_daemon::ai::provider(&st.cfg));
    let history = st.history.clone();
    let response = request(st,json!({"message":"Explain RAM"}),None).await;
    assert_eq!(response.status(),StatusCode::OK);
    let v: Value = serde_json::from_slice(&to_bytes(response.into_body(),100000).await.unwrap()).unwrap();
    assert_eq!(v["tier"],"local");
    assert!(v["reply"].as_str().unwrap().contains("Observed:"));
    assert!(history.lock().unwrap().recent_actions(10).unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn chat_opt_in_uses_key_without_enabling_automated_cloud_analysis() {
    use axum::{Json, Router, routing::post};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mock = Router::new().route("/v1/chat/completions",post(|headers: axum::http::HeaderMap, Json(v): Json<Value>| async move {
        assert_eq!(headers["authorization"],"Bearer test-only-key");
        assert_eq!(v["model"],"chat-only-model");
        assert!(v.get("tools").is_none());
        Json(json!({"choices":[{"message":{"content":"Observed: the largest process is using memory. A high percentage alone does not establish pressure."}}]}))
    }));
    let server = tokio::spawn(async move { axum::serve(listener,mock).await.unwrap() });
    let mut st = state();
    let cfg = Arc::make_mut(&mut st.cfg);
    cfg.assistant.cloud_enabled = true;
    cfg.assistant.cloud_model = Some("chat-only-model".into());
    cfg.nvidia.base_url = format!("http://{addr}");
    st.ai = Arc::new(ramwarden_daemon::ai::provider(&st.cfg));
    assert!(!st.ai.cloud_enabled);
    let response = request(st,json!({"message":"Why is RAM high?"}),None).await;
    assert_eq!(response.status(),StatusCode::OK);
    let v: Value = serde_json::from_slice(&to_bytes(response.into_body(),100000).await.unwrap()).unwrap();
    assert_eq!(v["tier"],"cloud");
    assert_eq!(v["facts"]["cloud_opt_in"],true);
    server.abort();
}

#[tokio::test]
async fn lightweight_status_and_health_do_not_wait_for_a_sampling_lock() {
    let st=state();
    let locked=st.det.write().unwrap();
    for path in ["/health","/companion/state"] {
        let response=tokio::time::timeout(std::time::Duration::from_millis(500),ramwarden_daemon::routes::router(st.clone()).oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())).await.expect("status must not wait on detector").unwrap();
        assert_eq!(response.status(),StatusCode::OK);
    }
    drop(locked);
}

#[tokio::test]
async fn proposed_fixes_require_a_single_use_confirmation_token() {
    use axum::{Json, Router, routing::post};
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr=listener.local_addr().unwrap();
    let mock=Router::new().route("/api/chat",post(|| async { Json(json!({"message":{"content":"{\"reply\":\"I can pause management for ten minutes. Your approval first.\",\"proposed_action\":{\"kind\":\"pause_management\"}}"}})) }));
    let server=tokio::spawn(async move { axum::serve(listener,mock).await.unwrap() });
    let mut st=state(); Arc::make_mut(&mut st.cfg).ollama.host=format!("http://{addr}");
    st.ai=Arc::new(ramwarden_daemon::ai::provider(&st.cfg));
    let app=ramwarden_daemon::routes::router(st.clone());
    let request=Request::builder().method("POST").uri("/assistant/chat").header("content-type","application/json").body(Body::from(json!({"message":"Pause automatic management"}).to_string())).unwrap();
    let response=app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(),StatusCode::OK);
    let v:Value=serde_json::from_slice(&to_bytes(response.into_body(),100000).await.unwrap()).unwrap();
    let token=v["proposed_action"]["token"].as_str().expect("proposal token");
    assert!(!st.on_hold(),"chat must not perform its proposed change");
    for (candidate,status) in [("not-a-token",StatusCode::CONFLICT),(token,StatusCode::OK),(token,StatusCode::CONFLICT)] {
        let request=Request::builder().method("POST").uri("/assistant/action").header("content-type","application/json").body(Body::from(json!({"token":candidate}).to_string())).unwrap();
        assert_eq!(app.clone().oneshot(request).await.unwrap().status(),status);
    }
    assert!(st.on_hold());
    server.abort();
}

#[tokio::test]
async fn free_ram_ignores_old_design_chat_and_reports_missing_browser() {
    let response = request(state(),json!({"message":"can you free more ram?","history":[
        {"role":"assistant","content":"Elongate the muzzle and reshape the horns."}
    ]}),None).await;
    let v:Value=serde_json::from_slice(&to_bytes(response.into_body(),100000).await.unwrap()).unwrap();
    assert_eq!(v["tier"],"facts");
    assert!(v["reply"].as_str().unwrap().contains("No browser is reporting"));
    assert!(!v["reply"].as_str().unwrap().contains("muzzle"));
    assert!(v["proposed_action"].is_null());
}

#[tokio::test]
async fn free_ram_proposes_only_eligible_tabs_without_executing() {
    use ramwarden_daemon::tabs::{Tab,Transport};
    let st=state();
    let tab=Tab { id:1,url:"https://example.org/article".into(),inactive_minutes:10000,
        active:Some(false),pinned:Some(false),audible:Some(false),discarded:Some(false),
        auto_discardable:Some(true),discard_supported:Some(true),status:Some("complete".into()),
        ..Default::default() };
    let mut active=tab.clone(); active.id=2; active.url="https://different.example/article".into(); active.active=Some(true);
    st.hub.lock().unwrap().reg.report("test-browser",Transport::Poll,vec![tab,active]);
    let response=request(st.clone(),json!({"message":"free more ram"}),None).await;
    let v:Value=serde_json::from_slice(&to_bytes(response.into_body(),100000).await.unwrap()).unwrap();
    assert_eq!(v["facts"]["eligible_idle_tabs"],1);
    assert!(v["proposed_action"]["token"].is_string());
    assert!(v["reply"].as_str().unwrap().contains("up to 1"));
    assert_eq!(st.hub.lock().unwrap().reg.queued_count("test-browser"),0);
    let mut hub=st.hub.lock().unwrap();
    let mut tabs=hub.reg.tabs_for("test-browser").to_vec();
    for tab in &mut tabs { tab.active=Some(true); }
    hub.reg.report("test-browser",Transport::Poll,tabs); drop(hub);
    let response=request(st,json!({"message":"free more ram"}),None).await;
    let v:Value=serde_json::from_slice(&to_bytes(response.into_body(),100000).await.unwrap()).unwrap();
    assert!(v["proposed_action"].is_null());
    assert!(v["reply"].as_str().unwrap().contains("no reported tabs currently qualify"));
}
