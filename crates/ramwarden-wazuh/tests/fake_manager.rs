//! The client against a fake Wazuh manager and indexer on loopback: sign-in,
//! token reuse, one re-sign-in on an expired token, and the alert search.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::Json;
use ramwarden_platform::Secret;
use ramwarden_wazuh::{Client, Endpoints, Error};
use serde_json::{Value, json};

#[derive(Default)]
struct Counts {
    signins: AtomicUsize,
    /// Reject this many agent lookups with 401, as an expired token would be.
    expire: AtomicUsize,
}

async fn serve(counts: Arc<Counts>) -> String {
    let c1 = counts.clone();
    let c2 = counts.clone();
    let app = Router::new()
        .route(
            "/security/user/authenticate",
            post(move |h: HeaderMap| {
                let c = c1.clone();
                async move {
                    // "wazuh-wui:pw" in base64.
                    if h.get("authorization").and_then(|v| v.to_str().ok()) != Some("Basic d2F6dWgtd3VpOnB3") {
                        return (StatusCode::UNAUTHORIZED, Json(json!({"error": 1})));
                    }
                    let n = c.signins.fetch_add(1, Ordering::SeqCst) + 1;
                    (StatusCode::OK, Json(json!({"data": {"token": format!("tok{n}")}, "error": 0})))
                }
            }),
        )
        .route(
            "/agents",
            get(move |h: HeaderMap| {
                let c = c2.clone();
                async move {
                    let auth = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
                    if !auth.starts_with("Bearer tok") {
                        return (StatusCode::UNAUTHORIZED, Json(json!({})));
                    }
                    if c.expire.load(Ordering::SeqCst) > 0 {
                        c.expire.fetch_sub(1, Ordering::SeqCst);
                        return (StatusCode::UNAUTHORIZED, Json(json!({})));
                    }
                    (StatusCode::OK, Json(json!({"data": {"affected_items": [
                        {"id": "003", "name": "desk", "status": "active"}
                    ]}, "error": 0})))
                }
            }),
        )
        .route(
            "/wazuh-alerts-*/_search",
            post(|Json(q): Json<Value>| async move {
                assert_eq!(q["query"]["bool"]["filter"][0]["term"]["agent.name"], "desk");
                Json(json!({"hits": {"hits": [
                    {"_source": {"timestamp": "t", "rule": {"id": "5712", "level": 12, "description": "brute force"}}}
                ]}}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn endpoints(base: &str, pw: &str) -> Endpoints {
    Endpoints {
        manager_url: base.to_string(),
        api_user: "wazuh-wui".into(),
        api_password: Secret::new(pw),
        indexer_url: base.to_string(),
        indexer_user: "admin".into(),
        indexer_password: Secret::new("x"),
        ca_pem: None,
        timeout: Duration::from_secs(5),
    }
}

#[tokio::test]
async fn signs_in_once_reuses_the_token_and_reads_agent_and_alerts() {
    let counts = Arc::new(Counts::default());
    let base = serve(counts.clone()).await;
    let c = Client::new(endpoints(&base, "pw")).unwrap();

    let a = c.agent("desk").await.unwrap().unwrap();
    assert_eq!(a.status, "active");
    assert!(c.agent("someone-else").await.unwrap().is_none());
    assert_eq!(counts.signins.load(Ordering::SeqCst), 1, "the token is reused");

    let alerts = c.alerts("desk", 7, 24).await.unwrap();
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].level, 12);
}

#[tokio::test]
async fn an_expired_token_is_refreshed_once() {
    let counts = Arc::new(Counts::default());
    let base = serve(counts.clone()).await;
    let c = Client::new(endpoints(&base, "pw")).unwrap();
    c.agent("desk").await.unwrap();
    counts.expire.store(1, Ordering::SeqCst);
    assert!(c.agent("desk").await.unwrap().is_some());
    assert_eq!(counts.signins.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn wrong_credentials_are_an_auth_error() {
    let base = serve(Arc::new(Counts::default())).await;
    let c = Client::new(endpoints(&base, "wrong")).unwrap();
    assert!(matches!(c.agent("desk").await, Err(Error::Auth("manager"))));
}

#[tokio::test]
async fn a_manager_that_is_down_is_unreachable_not_a_panic() {
    let c = Client::new(endpoints("http://127.0.0.1:1", "pw")).unwrap();
    assert!(matches!(c.agent("desk").await, Err(Error::Unreachable("manager", _))));
}

#[tokio::test]
async fn empty_or_invalid_id_scopes_do_not_make_a_request() {
    let c = Client::new(endpoints("http://127.0.0.1:1", "pw")).unwrap();
    assert!(c.agents_by_ids(&[]).await.unwrap().is_empty());
    assert!(c.alerts_by_ids(&[], 7, 24).await.unwrap().is_empty());
    for id in ["all", "000", "001,002", "1", "001&name=other", "99999999999"] {
        let ids = vec![id.to_string()];
        assert!(matches!(c.agents_by_ids(&ids).await, Err(Error::Parse(..))));
        assert!(matches!(c.alerts_by_ids(&ids, 7, 24).await, Err(Error::Parse(..))));
    }
    let too_many = vec!["001".to_string(); ramwarden_wazuh::api::MAX_ACCOUNT_AGENTS + 1];
    assert!(matches!(c.agents_by_ids(&too_many).await, Err(Error::Parse(..))));
}

#[tokio::test]
async fn manager_credentials_are_not_followed_through_a_redirect() {
    let followed = Arc::new(AtomicUsize::new(0));
    let counter = followed.clone();
    let app = Router::new()
        .route("/security/user/authenticate", post(|| async { axum::response::Redirect::temporary("/redirected") }))
        .route("/redirected", post(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Json(json!({"data": {"token": "should-not-be-used"}})) }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let c = Client::new(endpoints(&url, "pw")).unwrap();
    assert!(matches!(c.agents_by_ids(&["001".into()]).await, Err(Error::Status("manager", 307))));
    assert_eq!(followed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn oversized_upstream_data_is_refused() {
    let app = Router::new().route("/security/user/authenticate", post(|| async {
        Json(json!({"data": {"token": "x".repeat((1 << 20) + 1)}}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let c = Client::new(endpoints(&url, "pw")).unwrap();
    assert!(matches!(c.agents_by_ids(&["001".into()]).await, Err(Error::Parse("manager", _))));
}
