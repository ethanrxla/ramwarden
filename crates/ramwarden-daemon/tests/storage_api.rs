//! The storage warden over the real router: status, the agent's view, and
//! the rule that only the user's answer deletes.

use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use ramwarden_core::{actuator::Actuator, config::Config, detector::Detector, history::History, ladder::Ladder};
use ramwarden_daemon::hub::{AppState, Hub};
use ramwarden_daemon::storage::{Storage, catalog::Places};
use ramwarden_platform::{Volume, fake};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use tower::ServiceExt;

const MB: usize = 1_048_576;

fn write(path: &Path, bytes: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![9u8; bytes]).unwrap();
}

fn state(home: &Path, free_gb: f64) -> AppState {
    let cfg = Config::default();
    let world = Arc::new(Mutex::new(fake::World {
        volumes: vec![Volume { mount: "/".into(), name: "/".into(), total: 930 << 30, free: (free_gb * (1u64 << 30) as f64) as u64, system: true }],
        ..Default::default()
    }));
    let platform = Arc::new(fake::platform(world));
    AppState {
        cfg: Arc::new(cfg.clone()),
        det: Arc::new(RwLock::new(Detector::new(platform.processes()))),
        ladder: Arc::new(Mutex::new(Ladder::new(cfg.ladder.clone(), Actuator::for_platform(&platform)))),
        platform,
        hub: Arc::new(Mutex::new(Hub::new())),
        history: Arc::new(Mutex::new(History::open(Path::new(":memory:")).unwrap())),
        ai: Arc::new(ramwarden_daemon::ai::provider(&cfg)),
        models: Arc::new(Default::default()),
        predictor: Arc::new(ramwarden_daemon::predict::LivePredictor::new(std::env::temp_dir().join("ramwarden-test-no-predictor.safetensors"))),
        started: std::time::Instant::now(),
        hold_until: Default::default(),
        desktop: Arc::new(ramwarden_daemon::desktop::DesktopCtl::disabled()),
        voice: Default::default(),
        storage: Arc::new(Storage::new(Places { os: "linux", home: home.to_path_buf(), ..Default::default() })),
    }
}

async fn send(st: &AppState, method: &str, path: &str, body: Value) -> (u16, Value) {
    let response = ramwarden_daemon::routes::router(st.clone())
        .oneshot(Request::builder().method(method).uri(path).header("Content-Type", "application/json").body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, serde_json::from_slice(&to_bytes(response.into_body(), 1_000_000).await.unwrap()).unwrap_or(Value::Null))
}

#[tokio::test]
async fn status_reports_the_level_and_the_agent_view_hides_paths() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join(".cache/app/blob"), 60 * MB);
    let st = state(dir.path(), 0.2);
    let (code, status) = send(&st, "GET", "/storage", Value::Null).await;
    assert_eq!(code, 200);
    assert_eq!(status["level"], "critical");
    assert_eq!(status["volumes"][0]["free"], "0.2 GB");

    let (_, usage) = send(&st, "GET", "/storage/usage?for=agent", Value::Null).await;
    assert!(usage["home_folders"][0].get("path").is_none() && usage["places"][0].get("paths").is_none(), "{usage}");
    assert!(usage["summary"].as_str().unwrap().contains("/ has 0.2 GB free of 930 GB."));
    let (_, local) = send(&st, "GET", "/storage/usage", Value::Null).await;
    assert!(local["home_folders"][0]["path"].as_str().unwrap().starts_with(&*dir.path().to_string_lossy()));

    let (code, probe) = send(&st, "POST", "/ask/probe", json!({"probe":"storage"})).await;
    assert_eq!(code, 200);
    assert_eq!(probe["volumes"][0]["level"], "critical");
    assert!(!probe.to_string().contains("path"), "{probe}");
    assert_eq!(send(&st, "POST", "/ask/probe", json!({"probe":"shell","name":"rm -rf"})).await.0, 400);
}

#[tokio::test]
async fn an_agent_proposes_and_the_user_frees_from_the_bubble() {
    let dir = tempfile::tempdir().unwrap();
    let blob = dir.path().join(".cache/app/blob");
    write(&blob, 60 * MB);
    write(&dir.path().join("Documents/keep.txt"), 1_000);
    let st = state(dir.path(), 3.0);

    let (code, p) = send(&st, "POST", "/storage/cleanup/propose", json!({})).await;
    assert_eq!(code, 200, "{p}");
    assert_eq!(p["state"], "pending");
    assert_eq!(p["asked_in_bubble"], true);
    assert!(blob.exists(), "a proposal deletes nothing");
    let id = p["id"].as_str().unwrap().to_string();

    // The bubble carries the offer; "What's using it?" keeps it on offer.
    let (_, b) = send(&st, "GET", "/warden/bubble", Value::Null).await;
    assert_eq!(b["bubble"]["kind"], "storage");
    assert_eq!(b["bubble"]["proposal"], id.as_str());
    let labels: Vec<&str> = b["bubble"]["choices"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(labels, vec!["free", "show", "dismiss"]);
    assert!(b["bubble"]["sound"].is_null(), "an agent's proposal is not an alert");
    let (_, shown) = send(&st, "POST", "/warden/answer", json!({"id": b["bubble"]["id"], "choice": "show"})).await;
    assert!(shown["reply"].as_str().unwrap().contains("I can clear"), "{shown}");
    assert!(blob.exists());

    let (_, b) = send(&st, "GET", "/warden/bubble", Value::Null).await;
    let (_, freed) = send(&st, "POST", "/warden/answer", json!({"id": b["bubble"]["id"], "choice": "free"})).await;
    assert!(freed["reply"].as_str().unwrap().starts_with("Done. I cleared"), "{freed}");
    assert!(!blob.exists());
    assert!(dir.path().join("Documents/keep.txt").exists());

    let (_, after) = send(&st, "GET", &format!("/storage/cleanup/{id}"), Value::Null).await;
    assert_eq!(after["state"], "applied");
    assert!(after["freed_bytes"].as_u64().unwrap() >= 60 * MB as u64);
    assert_eq!(send(&st, "POST", &format!("/storage/cleanup/{id}/apply"), Value::Null).await.0, 409);
}

#[tokio::test]
async fn not_now_declines_and_nothing_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let blob = dir.path().join(".cache/app/blob");
    write(&blob, 60 * MB);
    let st = state(dir.path(), 3.0);
    let (_, p) = send(&st, "POST", "/storage/cleanup/propose", json!({})).await;
    let (_, b) = send(&st, "GET", "/warden/bubble", Value::Null).await;
    send(&st, "POST", "/warden/answer", json!({"id": b["bubble"]["id"], "choice": "dismiss"})).await;
    let (_, after) = send(&st, "GET", &format!("/storage/cleanup/{}", p["id"].as_str().unwrap()), Value::Null).await;
    assert_eq!(after["state"], "declined");
    assert_eq!(send(&st, "POST", &format!("/storage/cleanup/{}/apply", p["id"].as_str().unwrap()), Value::Null).await.0, 409);
    assert!(blob.exists());
}

#[tokio::test]
async fn a_due_alert_baas_once_with_the_offer() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join(".cache/app/blob"), 60 * MB);
    let st = state(dir.path(), 0.2);
    let now = std::time::Instant::now();
    let alert = st.storage.check(&st.cfg.storage, &st.platform, now, 1_000).expect("critical is due");
    // The same reading a minute later owes nothing.
    assert!(st.storage.check(&st.cfg.storage, &st.platform, now + std::time::Duration::from_secs(61), 1_061).is_none());
    ramwarden_daemon::storage::routes::raise(st.clone(), alert).await;
    let (_, b) = send(&st, "GET", "/warden/bubble", Value::Null).await;
    assert_eq!(b["bubble"]["sound"], "baa_critical");
    assert!(b["bubble"]["text"].as_str().unwrap().starts_with("/ is nearly full: 0.2 GB left of 930 GB."), "{b}");
    assert!(b["bubble"]["proposal"].is_string());
}

#[tokio::test]
async fn a_listener_beyond_this_computer_cannot_see_or_free_storage() {
    let dir = tempfile::tempdir().unwrap();
    let blob = dir.path().join(".cache/app/blob");
    write(&blob, 60 * MB);
    let local = state(dir.path(), 3.0);
    let (_, p) = send(&local, "POST", "/storage/cleanup/propose", json!({})).await;
    assert_eq!(p["state"], "pending");

    // Same daemon state, served on every interface.
    let mut cfg = (*local.cfg).clone();
    cfg.server.host = "0.0.0.0".into();
    let open = AppState { cfg: Arc::new(cfg), ..local.clone() };
    for path in ["/storage", "/storage/usage?for=agent", &format!("/storage/cleanup/{}", p["id"].as_str().unwrap())] {
        assert_ne!(send(&open, "GET", path, Value::Null).await.0, 200, "{path}");
    }
    assert_ne!(send(&open, "POST", "/ask/probe", json!({"probe": "storage"})).await.0, 200);
    let (_, b) = send(&local, "GET", "/warden/bubble", Value::Null).await;
    send(&open, "POST", "/warden/answer", json!({"id": b["bubble"]["id"], "choice": "free"})).await;
    assert!(blob.exists(), "Free is only ever approved from this computer");
}
