//! A lightweight status path: never wait behind detector or remediation locks.
use axum::{Json, extract::State};
use serde_json::{Value, json};
use crate::hub::AppState;

pub async fn status(State(st): State<AppState>) -> Json<Value> {
    let result = tokio::task::spawn_blocking(move || {
        let m = st.memory();
        let p = st.pressure();
        let mut out = json!({"ok":true,"percent":m.percent(),"used_mb":m.used() as f64/1048576.0,
            "total_mb":m.total as f64/1048576.0,"available_mb":m.available as f64/1048576.0,
            "psi_some":p.some_avg10,"rung":ramwarden_core::ladder::rung_for(&st.cfg.ladder,&p,m.available).as_str()});
        if let Ok(det) = st.det.try_read() {
            let rows = crate::ai::Snapshot::take(&det,&st.cfg.watchlist).processes;
            out["processes"] = json!(rows.into_iter().take(6).map(|(name,mb,_)|json!({"name":name,"rss_mb":mb})).collect::<Vec<_>>());
        }
        if let Ok(mut ladder) = st.ladder.try_lock() {
            out["suspended"] = json!(ladder.actuator().suspended().iter().map(|e|e.name.clone()).collect::<Vec<_>>());
        }
        out
    }).await.unwrap_or_else(|_|json!({"ok":false}));
    Json(result)
}
