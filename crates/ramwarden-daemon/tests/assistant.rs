use axum::{body::{Body, to_bytes}, http::{Request, StatusCode}};
use ramwarden_core::{actuator::Actuator, config::Config, detector::Detector, history::History, ladder::Ladder};
use ramwarden_daemon::hub::{AppState, Hub};
use ramwarden_platform::fake;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, RwLock};
use tower::ServiceExt;
fn state() -> AppState {
    let mut cfg = Config { db_path:std::env::temp_dir().join(format!("warden-assistant-test-{}",uuid::Uuid::new_v4())).join("history.db"), ..Default::default() };
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
        desktop: Arc::new(ramwarden_daemon::desktop::DesktopCtl::disabled()),
        voice: Default::default(),
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
// Holding the detector lock across the requests is the point of the test.
#[allow(clippy::await_holding_lock)]
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
    {
        let mut hub=st.hub.lock().unwrap();
        let mut tabs=hub.reg.tabs_for("test-browser").to_vec();
        for tab in &mut tabs { tab.active=Some(true); }
        hub.reg.report("test-browser",Transport::Poll,tabs);
    }
    let response=request(st,json!({"message":"free more ram"}),None).await;
    let v:Value=serde_json::from_slice(&to_bytes(response.into_body(),100000).await.unwrap()).unwrap();
    assert!(v["proposed_action"].is_null());
    assert!(v["reply"].as_str().unwrap().contains("no reported tabs currently qualify"));
}

#[tokio::test]
async fn threat_persona_does_not_inherit_memory_actions_or_bypass_its_pass() {
    let response=request(state(),json!({"message":"free ram and scan my files","warden":"threat"}),None).await;
    assert_eq!(response.status(),StatusCode::OK);
    let value:Value=serde_json::from_slice(&to_bytes(response.into_body(),100000).await.unwrap()).unwrap();
    assert!(value["proposed_action"].is_null());assert_eq!(value["tier"],"facts");
    assert!(value["reply"].as_str().unwrap().contains("Pro pass"));
    assert_eq!(request(state(),json!({"message":"hi","warden":"unknown"}),None).await.status(),StatusCode::BAD_REQUEST);
}

async fn post_route(app:axum::Router,path:&str,body:Value)->(StatusCode,Value){
    let response=app.oneshot(Request::builder().method("POST").uri(path).header("content-type","application/json").body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status=response.status();let bytes=to_bytes(response.into_body(),100000).await.unwrap();
    (status,serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}
#[tokio::test]
async fn workspace_tool_is_previewed_confirmed_once_and_invalidated_on_scope_change(){
    use axum::{Json,Router,routing::post};
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let addr=listener.local_addr().unwrap();
    let mock=Router::new().route("/api/chat",post(|Json(v):Json<Value>|async move{
        assert!(v["messages"].as_array().unwrap().iter().any(|m|m["content"].as_str().is_some_and(|s|s.contains("WORKSPACE TOOL CATALOG"))));
        Json(json!({"message":{"content":json!({"reply":"I can create notes.txt after your approval.","proposed_action":{"kind":"workspace","operation":{"tool":"create_text","path":"notes.txt","content":"fixture notes"}}}).to_string()}}))
    }));
    let server=tokio::spawn(async move{axum::serve(listener,mock).await.unwrap()});
    let folder=tempfile::tempdir().unwrap();let mut st=state();let cfg=Arc::make_mut(&mut st.cfg);
    cfg.ollama.host=format!("http://{addr}");cfg.assistant.workspace=Some(folder.path().to_path_buf());
    st.ai=Arc::new(ramwarden_daemon::ai::provider(&st.cfg));let app=ramwarden_daemon::routes::router(st);
    let (status,proposal)=post_route(app.clone(),"/assistant/chat",json!({"message":"create notes.txt with fixture notes"})).await;
    assert_eq!(status,StatusCode::OK);assert!(!folder.path().join("notes.txt").exists());
    assert!(proposal["proposed_action"]["preview"].as_str().unwrap().contains("fixture notes"));
    assert_eq!(proposal["proposed_action"]["auto_eligible"],true);
    assert!(!proposal["proposed_action"]["scope_id"].as_str().unwrap().is_empty());
    let token=proposal["proposed_action"]["token"].as_str().unwrap();
    let (status,result)=post_route(app.clone(),"/assistant/action",json!({"token":token})).await;
    assert_eq!(status,StatusCode::OK);assert!(result["reply"].as_str().unwrap().contains("bytes_written"));
    assert_eq!(std::fs::read_to_string(folder.path().join("notes.txt")).unwrap(),"fixture notes");
    assert_eq!(post_route(app.clone(),"/assistant/action",json!({"token":token})).await.0,StatusCode::CONFLICT);
    let (_,proposal)=post_route(app.clone(),"/assistant/chat",json!({"message":"create notes.txt"})).await;
    let token=proposal["proposed_action"]["token"].as_str().unwrap();
    assert_eq!(post_route(app.clone(),"/assistant/workspace",json!({"path":null})).await.0,StatusCode::OK);
    assert_eq!(post_route(app,"/assistant/action",json!({"token":token})).await.0,StatusCode::CONFLICT);
    server.abort();
}
#[tokio::test]
async fn unavailable_desktop_does_not_offer_or_claim_an_action(){
    let (status,result)=post_route(ramwarden_daemon::routes::router(state()),"/assistant/chat",json!({"message":"arrange my windows"})).await;
    assert_eq!(status,StatusCode::OK);assert!(result["proposed_action"].is_null());
    assert!(result["reply"].as_str().unwrap().contains("No desktop changes made"));
}
#[tokio::test]
async fn workspace_scope_rejects_network_listeners_and_web_origins(){
    let mut st=state();Arc::make_mut(&mut st.cfg).server.host="0.0.0.0".into();
    assert_eq!(post_route(ramwarden_daemon::routes::router(st),"/assistant/workspace",json!({"path":null})).await.0,StatusCode::NOT_FOUND);
    let response=ramwarden_daemon::routes::router(state()).oneshot(Request::builder().method("POST").uri("/assistant/workspace").header("content-type","application/json").header("origin","https://example.com").body(Body::from("{\"path\":null}")).unwrap()).await.unwrap();
    assert_eq!(response.status(),StatusCode::FORBIDDEN);
}
