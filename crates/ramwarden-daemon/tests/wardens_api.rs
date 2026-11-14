//! `/wardens`: the warden list, Pro gating, icons, and a locked warden doing
//! no work. See docs/specs/wardens.md.

use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use ramwarden_core::{
    actuator::Actuator, config::Config, detector::Detector, history::History, ladder::Ladder,
};
use ramwarden_daemon::hub::{AppState, Hub};
use ramwarden_platform::fake;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer, SigningKey};
use ramwarden_core::wardens::Entitlement;
use serde_json::Value;
use std::sync::{Arc, Mutex, RwLock};
use tower::ServiceExt;

fn state(dir: &std::path::Path) -> AppState {
    let cfg = Config { db_path: dir.join("history.db"), ..Config::default() };
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
        desktop: Arc::new(ramwarden_daemon::desktop::DesktopCtl::disabled()),
        voice: Default::default(),
    }
}

async fn get(st: &AppState, path: &str) -> (u16, String, Vec<u8>) {
    let r = ramwarden_daemon::routes::router(st.clone())
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = r.status().as_u16();
    let ctype = r
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    (status, ctype, to_bytes(r.into_body(), 1_000_000).await.unwrap().to_vec())
}

async fn post(st: &AppState, path: &str) -> u16 {
    ramwarden_daemon::routes::router(st.clone())
        .oneshot(Request::builder().method("POST").uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
        .as_u16()
}

async fn json(st: &AppState, path: &str) -> Value {
    let (status, _, body) = get(st, path).await;
    assert_eq!(status, 200, "{path}");
    serde_json::from_slice(&body).unwrap()
}

/// One test, because the daemon keeps one set of wardens per process and the
/// steps below change the plan on disk in order.
#[tokio::test]
async fn wardens_are_listed_gated_by_plan_and_drawn_from_their_icons() {
    let dir = tempfile::tempdir().unwrap();
    // A config dir with no device token in it: signed out, so nothing here
    // can reach the hosted API. The only test in this binary.
    // SAFETY: set before any other thread reads the environment.
    unsafe {
        std::env::set_var("APPDATA", dir.path());
    }
    let st = state(dir.path());
    let key = SigningKey::from_bytes(&[1; 32]);
    let w = ramwarden_daemon::wardens::Wardens::install(ramwarden_daemon::wardens::Wardens::with_public_key(
        &st.cfg,
        Some(B64.encode(key.verifying_key().as_bytes())),
    ));

    // Free tier: RAM works, Wazuh is locked and has no report.
    let list = json(&st, "/wardens").await;
    assert_eq!(list["paid"], serde_json::json!([]));
    let ids: Vec<&str> = list["wardens"].as_array().unwrap().iter().map(|w| w["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["ram", "threat"]);
    let ram = &list["wardens"][0];
    assert_eq!(ram["access"], "free");
    assert_eq!(ram["unlocked"], true);
    assert_ne!(ram["status"]["level"], "locked");
    let threat = &list["wardens"][1];
    assert_eq!(threat["access"], "pro");
    assert_eq!(threat["unlocked"], false);
    assert_eq!(threat["status"]["level"], "locked");
    assert_eq!(threat["icon"], "/wardens/threat/icon.svg");
    assert_eq!(threat["price"], "In RamWarden Pro, from $5 a month");
    assert_eq!(threat["upgrade_url"], "https://ramwarden.com/#pricing");
    assert!(ram["price"].is_null() && ram["upgrade_url"].is_null());
    assert!(json(&st, "/wardens/threat").await["report"].is_null());

    // A locked warden cannot be set up.
    assert_eq!(post(&st, "/wardens/threat/setup").await, 403);

    // A RAM (hosted Jev) pass does not unlock it; a ThreatWarden pass does.
    let until = ramwarden_core::wardens::now() + 3600;
    let pass = |id: &str| {
        let mut e = Entitlement {
            v: 2,
            tier: Some("pro".into()),
            status: Some("active".into()),
            account: Some("acct1".into()),
            device: Some("dev1".into()),
            issued_at: until - 3600,
            ..Default::default()
        };
        e.wardens.insert(id.to_string(), until);
        e.sig = Some(B64.encode(key.sign(e.signed_text().as_bytes()).to_bytes()));
        serde_json::to_string(&e).unwrap()
    };
    // An unsigned pass unlocks nothing.
    std::fs::write(dir.path().join("entitlement.json"), format!(r#"{{"v":2,"tier":"pro","wardens":{{"threat":{until}}}}}"#)).unwrap();
    assert_eq!(json(&st, "/wardens").await["wardens"][1]["unlocked"], false);
    std::fs::write(dir.path().join("entitlement.json"), pass("ram")).unwrap();
    assert_eq!(json(&st, "/wardens").await["wardens"][1]["unlocked"], false);
    std::fs::write(dir.path().join("entitlement.json"), pass("threat")).unwrap();
    let list = json(&st, "/wardens").await;
    assert_eq!(list["paid"], serde_json::json!(["threat"]));
    assert_eq!((list["tier"].as_str(), list["plan_status"].as_str()), (Some("pro"), Some("active")));
    assert_eq!(list["wardens"][1]["unlocked"], true);

    // Unlocked but this computer is signed out: hosted mode says so, never
    // "clear" about alerts it could not see, and setup is refused.
    w.threat.poll(None).await;
    let one = json(&st, "/wardens/threat").await;
    assert!(one["report"].is_object(), "{one}");
    assert_eq!(one["report"]["mode"], "hosted");
    assert_eq!(one["status"]["level"], "off");
    assert_eq!(one["status"]["headline"], "Sign in to RamWarden");
    assert_eq!(post(&st, "/wardens/threat/setup").await, 409);
    assert_eq!(post(&st, "/wardens/ram/setup").await, 400);

    // Icons: the RAM warden is the brand ram the clients already draw.
    assert!(list["wardens"][0]["icon"].is_null());
    let (status, ctype, body) = get(&st, "/wardens/threat/icon.svg").await;
    assert_eq!(status, 200);
    assert_eq!(ctype, "image/svg+xml");
    assert!(String::from_utf8(body).unwrap().contains("<svg"));
    assert_eq!(get(&st, "/wardens/ram/icon.svg").await.0, 404);
    assert_eq!(get(&st, "/wardens/nope/icon.svg").await.0, 404);
    assert_eq!(get(&st, "/wardens/nope").await.0, 404);
}
