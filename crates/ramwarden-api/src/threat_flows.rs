//! Real HTTP Wazuh fixtures behind the hosted router, including deliberately
//! overbroad upstream responses. No live credentials or production accounts.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::Query;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use ramwarden_cloud::model::{BillingUpdate, Device, Platform, ThreatAgentBinding, Tier};
use ramwarden_cloud::store::Store;
use ramwarden_platform::Secret;
use ramwarden_wazuh::Endpoints;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::flows::{self, App, Auth, NOW};
use crate::threat::{Config, ThreatService};
use crate::wallet::testing::EvmWallet;

const STATUS: &str = "/v1/wardens/threat/status";

#[derive(Default)]
struct Calls {
    queries: Mutex<Vec<Value>>,
    mode: AtomicUsize,
    hold: AtomicBool,
    entered: Notify,
    release: Notify,
}

async fn fixture() -> (App, Arc<Calls>) {
    let calls = Arc::new(Calls::default());
    let manager = calls.clone();
    let indexer = calls.clone();
    let upstream = Router::new()
        .route("/security/user/authenticate", post(|headers: HeaderMap| async move {
            assert_eq!(headers["authorization"], "Basic cmVhZGVyOnB3");
            Json(json!({"data": {"token": "fixture-manager-token"}, "error": 0}))
        }))
        .route("/agents", get(move |Query(query): Query<std::collections::HashMap<String, String>>, headers: HeaderMap| {
            let calls = manager.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer fixture-manager-token");
                assert!(!query["agents_list"].is_empty());
                assert!(!query.contains_key("name"));
                calls.queries.lock().unwrap().push(json!({"manager": query["agents_list"]}));
                let agents = if calls.mode.load(Ordering::SeqCst) == 3 { json!([]) } else { json!([
                    {"id": "001", "name": "same-hostname", "status": "active", "lastKeepAlive": "2026-11-03T10:00:00Z"},
                    {"id": "002", "name": "same-hostname", "status": "disconnected"},
                    {"id": "009", "name": "unrelated-private-computer", "status": "active"}
                ]) };
                Json(json!({"data": {"affected_items": agents}, "error": 0}))
            }
        }))
        .route("/wazuh-alerts-*/_search", post(move |headers: HeaderMap, Json(query): Json<Value>| {
            let calls = indexer.clone();
            async move {
                assert_eq!(headers["authorization"], "Basic cmVhZGVyOnB3");
                assert!(query["query"]["bool"]["filter"][0]["terms"]["agent.id"].is_array());
                assert!(query["_source"].as_array().unwrap().contains(&json!("agent.id")));
                calls.queries.lock().unwrap().push(query);
                if calls.hold.load(Ordering::SeqCst) {
                    calls.entered.notify_one();
                    calls.release.notified().await;
                }
                match calls.mode.load(Ordering::SeqCst) {
                    1 => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "indexer fixture private detail"}))),
                    2 => (StatusCode::OK, Json(json!({"timed_out": true, "hits": {"hits": []}}))),
                    _ => (StatusCode::OK, Json(json!({"hits": {"hits": [
                        {"_source": {"agent": {"id": "009"}, "rule": {"level": 15, "description": "foreign secret"}}},
                        {"_source": {"agent": {"id": "001"}, "rule": {"id": "1", "level": 9, "description": "account one"}, "full_log": "private raw log"}},
                        {"_source": {"agent": {"id": "002"}, "rule": {"id": "2", "level": 8, "description": "account two"}}},
                        {"_source": {"rule": {"level": 14, "description": "unscoped secret"}}}
                    ]}}))),
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
    let service = ThreatService::new(Config {
        deployment: "fixture".into(),
        ca_file: None,
        endpoints: Endpoints {
            manager_url: url.clone(),
            indexer_url: url,
            api_user: "reader".into(),
            api_password: Secret::new("pw"),
            indexer_user: "reader".into(),
            indexer_password: Secret::new("pw"),
            ca_pem: None,
            timeout: Duration::from_secs(2),
        },
    })
    .unwrap();
    (flows::app_with_threat(Some(service)), calls)
}

async fn account(a: &App, seed: u8, pro: bool) -> (String, String, Device, String) {
    let (cookie, id) = a.web(&EvmWallet::new(seed)).await;
    if pro {
        a.store
            .update_billing(
                &id,
                &BillingUpdate {
                    tier: Some(Tier::Pro),
                    paid_until: Some(NOW + 86400),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    let (device, token) = a
        .store
        .register_device(&id, "test device", Platform::Macos, NOW)
        .await
        .unwrap()
        .unwrap();
    (cookie, id, device, token)
}

async fn bind(a: &App, device: &Device, deployment: &str, agent_id: &str) {
    assert!(
        a.store
            .bind_threat_agent(&ThreatAgentBinding {
                account_id: device.account_id.clone(),
                device_id: device.id.clone(),
                deployment: deployment.into(),
                agent_id: agent_id.into(),
                created_at: NOW,
            })
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn status_requires_a_live_pro_device_not_a_web_session_or_operator_token() {
    let (a, calls) = fixture().await;
    let (cookie, id, device, token) = account(&a, 80, false).await;
    for auth in [
        Auth::None,
        Auth::Cookie(&cookie),
        Auth::Bearer(&cookie),
        Auth::Bearer("operator-token"),
    ] {
        assert_eq!(
            a.call("GET", STATUS, auth, None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        a.call("GET", STATUS, Auth::Bearer(&token), None).await.0,
        StatusCode::PAYMENT_REQUIRED
    );
    a.store
        .update_billing(
            &id,
            &BillingUpdate {
                tier: Some(Tier::Ram),
                paid_until: Some(NOW + 100),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        a.call("GET", STATUS, Auth::Bearer(&token), None).await.0,
        StatusCode::PAYMENT_REQUIRED
    );
    a.store
        .update_billing(
            &id,
            &BillingUpdate {
                tier: Some(Tier::Pro),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    bind(&a, &device, "fixture", "001").await;
    a.store.revoke_device(&id, &device.id).await.unwrap();
    assert_eq!(
        a.call("GET", STATUS, Auth::Bearer(&token), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert!(calls.queries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn account_bindings_filter_both_queries_and_overbroad_responses() {
    let (a, calls) = fixture().await;
    let (_, one, d1, t1) = account(&a, 81, true).await;
    let (_, two, d2, t2) = account(&a, 82, true).await;
    bind(&a, &d1, "fixture", "001").await;
    bind(&a, &d2, "fixture", "002").await;
    let (other_deployment, _) = a
        .store
        .register_device(&one, "other deployment", Platform::Linux, NOW)
        .await
        .unwrap()
        .unwrap();
    bind(&a, &other_deployment, "different-deployment", "009").await;
    let target = format!("{STATUS}?account_id={two}&agent_id=002&agents_list=009");
    let (s, v, headers) = a
        .raw("GET", &target, Auth::Bearer(&t1), flows::ORIGIN, None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(v["agents"].as_array().unwrap().len(), 1);
    assert_eq!(v["agents"][0]["id"], "001");
    assert_eq!(v["alerts"].as_array().unwrap().len(), 1);
    assert_eq!(v["alerts"][0]["description"], "account one");
    assert!(
        v["alerts"][0].get("verdict").is_none(),
        "no invented model verdict"
    );
    let text = v.to_string();
    for secret in [
        "account two",
        "foreign secret",
        "unscoped secret",
        "private raw log",
        "fixture-manager-token",
    ] {
        assert!(!text.contains(secret), "leaked {secret}");
    }
    let (_, other) = a.call("GET", STATUS, Auth::Bearer(&t2), None).await;
    assert_eq!(other["alerts"][0]["description"], "account two");
    let queries = calls.queries.lock().unwrap().clone();
    assert_eq!(queries[0]["manager"], "001");
    assert_eq!(
        queries[1]["query"]["bool"]["filter"][0]["terms"]["agent.id"],
        json!(["001"])
    );
    assert_eq!(queries[2]["manager"], "002");
}

#[tokio::test]
async fn no_bindings_and_revoked_bindings_never_query_all_agents() {
    let (a, calls) = fixture().await;
    let (_, id, first, token) = account(&a, 83, true).await;
    let (s, empty) = a.call("GET", STATUS, Auth::Bearer(&token), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(empty["agents"].as_array().unwrap().is_empty());
    assert!(
        empty["summary"]
            .as_str()
            .unwrap()
            .contains("No security agents are linked")
    );
    bind(&a, &first, "fixture", "001").await;
    let (_, second_token) = a
        .store
        .register_device(&id, "second device", Platform::Windows, NOW)
        .await
        .unwrap()
        .unwrap();
    a.store.revoke_device(&id, &first.id).await.unwrap();
    assert_eq!(
        a.call("GET", STATUS, Auth::Bearer(&second_token), None)
            .await
            .1["agents"],
        json!([])
    );
    assert!(calls.queries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unavailable_or_incomplete_security_data_is_not_reported_as_clear() {
    let (a, calls) = fixture().await;
    let (_, _, device, token) = account(&a, 84, true).await;
    bind(&a, &device, "fixture", "001").await;
    for mode in [1, 2, 3] {
        calls.mode.store(mode, Ordering::SeqCst);
        let (s, v) = a.call("GET", STATUS, Auth::Bearer(&token), None).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
        assert!(!v.to_string().contains("private detail"));
        assert!(v.get("alerts").is_none());
    }
    let unconfigured = flows::app();
    let (_, _, _, token) = account(&unconfigured, 85, true).await;
    assert_eq!(
        unconfigured
            .call("GET", STATUS, Auth::Bearer(&token), None)
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn revoking_a_device_during_an_upstream_request_prevents_release() {
    let (a, calls) = fixture().await;
    let (_, id, device, token) = account(&a, 86, true).await;
    bind(&a, &device, "fixture", "001").await;
    calls.hold.store(true, Ordering::SeqCst);
    let revoke = async {
        calls.entered.notified().await;
        a.store.revoke_device(&id, &device.id).await.unwrap();
        calls.release.notify_one();
    };
    let ((s, v), ()) = tokio::join!(a.call("GET", STATUS, Auth::Bearer(&token), None), revoke);
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert!(!v.to_string().contains("account one"));
}

#[tokio::test]
async fn polling_is_limited_per_account_across_device_tokens() {
    let (a, calls) = fixture().await;
    let (_, id, _, first) = account(&a, 87, true).await;
    let (_, second) = a
        .store
        .register_device(&id, "second", Platform::Windows, NOW)
        .await
        .unwrap()
        .unwrap();
    for i in 0..30 {
        let token = if i % 2 == 0 { &first } else { &second };
        assert_eq!(
            a.call("GET", STATUS, Auth::Bearer(token), None).await.0,
            StatusCode::OK
        );
    }
    let (s, _, h) = a
        .raw("GET", STATUS, Auth::Bearer(&second), flows::ORIGIN, None)
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(h["retry-after"], "60");
    assert!(calls.queries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn revoking_another_bound_device_during_the_request_prevents_its_alerts_being_returned() {
    let (a, calls) = fixture().await;
    let (_, id, first, _) = account(&a, 88, true).await;
    bind(&a, &first, "fixture", "001").await;
    let (_, second_token) = a
        .store
        .register_device(&id, "second", Platform::Windows, NOW)
        .await
        .unwrap()
        .unwrap();
    calls.hold.store(true, Ordering::SeqCst);
    let revoke = async {
        calls.entered.notified().await;
        a.store.revoke_device(&id, &first.id).await.unwrap();
        calls.release.notify_one();
    };
    let ((s, v), ()) = tokio::join!(
        a.call("GET", STATUS, Auth::Bearer(&second_token), None),
        revoke
    );
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert!(!v.to_string().contains("account one"));
}
