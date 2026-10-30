//! The HTTP surface, reproducing v1's `:7823` API.
//!
//! The browser extensions are shipped, installed, and out of reach, so
//! `POST /api/tabs` and the `/ws` protocol are fixed contracts — they are
//! reproduced exactly. The rest of v1's endpoints were consumed by `curl` and by
//! the GTK window, so they keep their shapes but report PSS where v1 reported
//! RSS, which is the whole point of the rewrite.
//!
//! New in v2: `/state` (what the window needs in one call, so the Python UI can
//! run out of process), `/psi`, `/ladder/plan`, `/ladder/cancel-kill`, and
//! `/actions`.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Router, response::IntoResponse};

use ramwarden_core::signals::Verdict;
use ramwarden_core::{ladder, workspaces};
use ramwarden_ai::vram;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::extras;
use crate::hub::AppState;
use crate::tabs::{Tab, Transport};

pub fn router(state: AppState) -> Router {
    let policy = crate::guard::Policy::for_bind(&state.cfg.server.host, state.cfg.server.port);
    Router::new()
        // ── Contracts the shipped extensions depend on ──────────────────────
        .route("/api/tabs", post(poll_tabs))
        .route("/ws", get(crate::ws::handler))
        // ── v1 endpoints ────────────────────────────────────────────────────
        .route("/", get(index))
        .route("/health", get(health))
        .route("/stats", get(stats))
        .route("/ram-report", get(ram_report))
        .route("/apps", get(apps))
        .route("/activity", get(activity))
        .route("/activity/{name}", get(activity_one))
        .route("/suspend/{name}", post(suspend))
        .route("/kill/{name}", post(kill))
        .route("/resume/{name}", post(resume))
        .route("/suspended", get(suspended))
        .route("/watchlist", get(watchlist))
        .route("/terminals", get(list_terminals))
        .route("/terminals/close-idle", post(close_idle_terminals))
        .route("/workspaces", get(get_workspaces))
        .route("/workspaces/sort", post(sort_workspaces))
        .route("/analyze", post(analyze))
        .route("/browser/tabs", get(browser_tabs))
        .route("/browser/analyze", post(browser_analyze))
        .route("/browser/discard", post(browser_discard))
        .route("/browser/close", post(browser_close))
        .route("/history", get(history).delete(clear_history))
        .route("/debug/tabs", get(debug_tabs))
        // ── v2 additions ────────────────────────────────────────────────────
        .route("/state", get(window_state))
        .route("/psi", get(pressure))
        .route("/ladder/plan", get(ladder_plan))
        .route("/ladder/cancel-kill", post(cancel_kill))
        .route("/ladder/undo", post(undo_last))
        .route("/lru", get(lru))
        .route("/actions", get(actions))
        .route("/outcomes", get(outcomes))
        .route("/intel", get(intel))
        .route("/hold/{secs}", post(hold))
        .route("/hold", delete(release_hold))
        .route("/tidy", get(tidy))
        .route("/predict/eval", get(predict_eval))
        .route("/predict/train", post(predict_train))
        .route("/gpu", get(gpu_report))
        .route("/gpu/unload", post(gpu_unload))
        .route("/debloat", get(debloat_status))
        .route("/debloat/apply", post(debloat_apply))
        .route("/debloat/revert", post(debloat_revert))
        .route("/ai", get(ai_status))
        .route("/signals", get(signals))
        .route("/reclaim/{scope}", post(reclaim_scope))
        .route("/tabs/close", post(close_tabs_route))
        .route("/history/clear", delete(clear_history))
        .with_state(state)
        // Outermost last: CORS answers allowed preflights, the guard refuses
        // everything a web page might send. See `guard` for why both.
        .layer(axum::middleware::from_fn_with_state(policy.clone(), crate::guard::enforce))
        .layer(policy.cors())
}

pub(crate) fn mb(bytes: u64) -> f64 {
    (bytes as f64 / (1024.0 * 1024.0) * 10.0).round() / 10.0
}

// ── Extension contracts ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TabPollBody {
    browser_id: String,
    #[serde(default)]
    tabs: Vec<Tab>,
}

/// Firefox polls here every 30 seconds. The response carries any queued
/// commands, which the extension executes immediately.
async fn poll_tabs(State(st): State<AppState>, Json(body): Json<TabPollBody>) -> Json<Value> {
    let mut hub = st.hub.lock().unwrap();
    hub.reg.report(&body.browser_id, Transport::Poll, body.tabs);
    let commands = hub.reg.take_queued(&body.browser_id);
    if !commands.is_empty() {
        tracing::info!(
            "delivering {} queued command(s) to poll browser [{}]",
            commands.len(),
            &body.browser_id[..body.browser_id.len().min(8)]
        );
    }
    Json(json!({"commands": commands, "browser_id": body.browser_id}))
}

/// What a person sees if they open the daemon's address in a browser: this
/// is the background service, and the app is elsewhere.
async fn index(State(st): State<AppState>) -> axum::response::Html<String> {
    let m = st.memory();
    axum::response::Html(format!(
        r#"<!doctype html><meta charset="utf-8"><title>RamWarden</title>
<style>body{{font:15px system-ui;max-width:36em;margin:3em auto;padding:0 1em;color:#222}}
code{{background:#eee;padding:.1em .3em;border-radius:3px}}</style>
<h1>RamWarden is running</h1>
<p>This address is RamWarden's background service. It has no window of its own.</p>
<p><b>To open the app:</b> Start menu &rarr; <b>RamWarden</b>, or click the ring icon
in the system tray (it may be under the <code>^</code> arrow by the clock).</p>
<p>Memory now: {used:.1} GB of {total:.1} GB in use.</p>
<p>Status for tools and scripts: <a href="/state">/state</a>, <a href="/psi">/psi</a>,
<a href="/tidy">/tidy</a>, <a href="/gpu">/gpu</a>, <a href="/predict/eval">/predict/eval</a>.</p>"#,
        used = m.used() as f64 / 1e9,
        total = m.total as f64 / 1e9,
    ))
}

async fn health(State(st): State<AppState>) -> Json<Value> {
    let det = st.det.read().unwrap();
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "warm": det.is_warm(),
        "uptime_seconds": st.started.elapsed().as_secs(),
    }))
}

async fn debug_tabs(State(st): State<AppState>) -> Json<Value> {
    let hub = st.hub.lock().unwrap();
    let describe = |ids: Vec<String>| -> Value {
        let mut map = serde_json::Map::new();
        for id in ids {
            let tabs = hub.reg.tabs_for(&id);
            map.insert(
                id.clone(),
                json!({
                    "tab_count": tabs.len(),
                    "incognito_count": tabs.iter().filter(|t| t.incognito).count(),
                    "sample": tabs.iter().take(3).collect::<Vec<_>>(),
                }),
            );
        }
        Value::Object(map)
    };
    Json(json!({
        "ws_browsers": describe(hub.reg.socket_ids()),
        "poll_browsers": describe(hub.reg.poll_ids()),
    }))
}

// ── Memory and activity ─────────────────────────────────────────────────────

async fn stats(State(st): State<AppState>) -> Json<Value> {
    let m = st.memory();
    let det = st.det.read().unwrap();
    let hub = st.hub.lock().unwrap();
    let terms = extras::terminals();

    let mut procs: Vec<_> = det.snapshot().values().collect();
    procs.sort_by_key(|s| std::cmp::Reverse(s.pss));

    Json(json!({
        "percent": m.percent(),
        "used_mb": mb(m.used()),
        "total_mb": mb(m.total),
        "processes": procs.iter().take(15).map(|s| json!({
            "name": s.name, "pid": s.pid, "rss_mb": mb(s.pss), "status": s.state.to_string(),
        })).collect::<Vec<_>>(),
        "browsers_connected": hub.reg.browsers_connected(),
        "ws_browsers": hub.reg.socket_count(),
        "poll_browsers": hub.reg.poll_count(),
        "private_windows": extras::private_windows(),
        "terminals": extras::terminals_json(&terms),
    }))
}

/// A breakdown of what memory is doing, and how far v1 would have been wrong.
async fn ram_report(State(st): State<AppState>) -> Json<Value> {
    let m = st.memory();
    let z = st.platform.memory.compression().unwrap_or_default();
    let det = st.det.read().unwrap();
    let totals = det.totals();

    let mut procs: Vec<_> = det.snapshot().values().collect();
    procs.sort_by_key(|s| std::cmp::Reverse(s.pss));
    let summed_rss: u64 = procs.iter().map(|s| s.rss).sum();
    let summed_pss: u64 = procs.iter().map(|s| s.pss).sum();

    Json(json!({
        "total_mb": mb(m.total),
        "used_mb": mb(m.used()),
        "available_mb": mb(m.available),
        "swap_used_mb": mb(m.swap_used()),
        // In-RAM compression: zram on Linux, the compression store on Windows.
        // The key keeps its v1 name because the window reads it.
        "zram": {
            "stored_mb": mb(z.original),
            "ram_cost_mb": mb(z.resident),
            "ratio": (z.ratio() * 100.0).round() / 100.0,
            // What a naive "used memory" reading overstates by.
            "saved_mb": mb(z.saved()),
        },
        "by_verdict_mb": {
            "PROTECTED": mb(totals["PROTECTED"]),
            "IN_USE": mb(totals["IN_USE"]),
            "IDLE": mb(totals["IDLE"]),
        },
        "accounting": {
            "summed_pss_mb": mb(summed_pss),
            "summed_rss_mb": mb(summed_rss),
            // v1 summed RSS. This is the factor by which it overstated.
            "v1_overstatement": if summed_pss > 0 {
                (summed_rss as f64 / summed_pss as f64 * 100.0).round() / 100.0
            } else { 1.0 },
            // False means PSS could not be read and these figures ARE summed
            // RSS — the overcount this rewrite exists to remove.
            "pss_available": det.pss_available(),
        },
        "top": procs.iter().take(20).map(|s| json!({
            "name": s.name, "pid": s.pid, "pss_mb": mb(s.pss), "rss_mb": mb(s.rss),
            "verdict": s.verdict.as_str(), "role": s.role.as_str(),
            "scope": s.scope, "reasons": s.reasons,
            "app": s.app, "part": s.part,
            "label": ramwarden_core::describe::label(
                s, det.quiet_minutes(&s.app, std::time::Instant::now())),
        })).collect::<Vec<_>>(),
    }))
}

/// Memory by application: how much of each app is in use, idle or protected, what
/// parts it is made of, and how long it has been quiet. This is the view for
/// "what is taking RAM that nobody is using".
async fn apps(State(st): State<AppState>) -> Json<Value> {
    let det = st.det.read().unwrap();
    let all = ramwarden_core::apps::from_detector(&det, std::time::Instant::now());
    let idle: f64 = all.iter().map(|a| a.idle_mb).sum();
    let in_use: f64 = all.iter().map(|a| a.in_use_mb).sum();
    let prot: f64 = all.iter().map(|a| a.protected_mb).sum();
    Json(json!({
        "totals_mb": { "in_use": in_use.round(), "idle": idle.round(), "protected": prot.round() },
        "apps": all.into_iter().take(30).collect::<Vec<_>>(),
    }))
}

async fn activity(State(st): State<AppState>) -> Json<Value> {
    let det = st.det.read().unwrap();
    let totals = det.totals();
    let mut sigs: Vec<_> = det.snapshot().values().collect();
    sigs.sort_by_key(|s| std::cmp::Reverse(s.pss));

    let group = |v: Verdict| -> Vec<Value> {
        sigs.iter()
            .filter(|s| s.verdict == v && s.pss >= 50 * 1024 * 1024)
            .take(25)
            .map(|s| signal_json(s))
            .collect()
    };

    Json(json!({
        "warm": det.is_warm(),
        "totals_mb": {
            "PROTECTED": mb(totals["PROTECTED"]),
            "IN_USE": mb(totals["IN_USE"]),
            "IDLE": mb(totals["IDLE"]),
        },
        "by_verdict": {
            "PROTECTED": group(Verdict::Protected),
            "IN_USE": group(Verdict::InUse),
            "IDLE": group(Verdict::Idle),
        },
        "reclaimable": det.reclaimable(&st.cfg.watchlist).iter()
            .map(|s| signal_json(s)).collect::<Vec<_>>(),
        "profiled": sigs.len(),
    }))
}

/// One process's signals, in v1's `as_dict` shape.
///
/// `rss_mb` carries PSS. The name is kept because the GTK window reads it, and
/// renaming the field would be a change to `window.py` — but the number is now
/// the honest one.
fn signal_json(s: &ramwarden_core::signals::Signals) -> Value {
    json!({
        "pid": s.pid,
        "name": s.name,
        "rss_mb": mb(s.pss),
        "pss_mb": mb(s.pss),
        "true_rss_mb": mb(s.rss),
        "status": s.state.to_string(),
        "role": s.role.as_str(),
        "verdict": s.verdict.as_str(),
        "protection": match s.protection {
            ramwarden_core::signals::Protection::None => "",
            ramwarden_core::signals::Protection::Structural => "structural",
            ramwarden_core::signals::Protection::Serving => "serving",
        },
        "reasons": s.reasons,
        "listening_ports": s.listening_ports,
        "established": s.established,
        "focused": s.is_focused,
        "window": s.has_window,
        "audio": s.playing_audio,
        "tty": s.has_tty,
        "cpu_seconds_recent": (s.cpu_seconds_recent * 100.0).round() / 100.0,
        "age_minutes": (s.age_minutes * 10.0).round() / 10.0,
        "active_descendant": s.active_descendant,
        "scope": s.scope,
    })
}

async fn activity_one(State(st): State<AppState>, Path(name): Path<String>) -> Json<Value> {
    let det = st.det.read().unwrap();
    let (verdict, reasons, pids) = det.verdict_for_name(&name);
    let decision = det.may_suspend(&name, &st.cfg.watchlist);
    Json(json!({
        "name": name,
        "verdict": verdict.as_str(),
        "reasons": reasons,
        "pids": pids,
        "may_suspend": decision.is_allowed(),
        "why": decision.reason(),
        "processes": det.matching(&name).iter().map(|s| signal_json(s)).collect::<Vec<_>>(),
    }))
}

async fn pressure(State(st): State<AppState>) -> Json<Value> {
    let p = st.pressure();
    let available = ladder::available_bytes(st.platform.memory.as_ref());
    let rung = ladder::rung_for(&st.cfg.ladder, &p, available);
    Json(json!({
        "some": {"avg10": p.some_avg10, "avg60": p.some_avg60, "avg300": p.some_avg300},
        "full": {"avg10": p.full_avg10, "avg60": p.full_avg60, "avg300": p.full_avg300},
        // True on Windows, where there is no PSI and this is a fault/commit
        // estimate on the same scale.
        "estimated": p.estimated,
        "available_mb": mb(st.memory().available),
        "rung": rung.as_str(),
    }))
}

// ── Actions ─────────────────────────────────────────────────────────────────

/// `force=true` skips the watchlist and in-use checks, for a user who clicked a
/// specific row and meant it. It never skips structural protection — there is no
/// gesture in any UI that should be able to freeze the compositor.
#[derive(Deserialize, Default)]
struct ForceQuery {
    #[serde(default)]
    force: bool,
}

async fn suspend(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<ForceQuery>,
) -> impl IntoResponse {
    let det = st.det.read().unwrap();
    let mut ladder = st.ladder.lock().unwrap();
    let outcome = ladder
        .actuator()
        .suspend(&name, &det, &st.cfg.watchlist, q.force);
    let code = if outcome.did_nothing() {
        StatusCode::CONFLICT
    } else {
        StatusCode::OK
    };
    (
        code,
        Json(json!({
            "name": name,
            "forced": q.force,
            "suspended": outcome.affected,
            "freed_mb": mb(outcome.bytes_freed),
            "notes": outcome.notes,
        })),
    )
}

/// Terminate, then kill. Same gate as suspend: structural protection is absolute.
async fn kill(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<ForceQuery>,
) -> impl IntoResponse {
    let det = st.det.read().unwrap();
    let mut ladder = st.ladder.lock().unwrap();
    let outcome = ladder.actuator().kill(&name, &det, &st.cfg.watchlist, q.force);
    let code = if outcome.did_nothing() {
        StatusCode::CONFLICT
    } else {
        StatusCode::OK
    };
    (
        code,
        Json(json!({
            "name": name,
            "forced": q.force,
            "killed": outcome.affected,
            "freed_mb": mb(outcome.bytes_freed),
            "notes": outcome.notes,
        })),
    )
}

async fn resume(State(st): State<AppState>, Path(name): Path<String>) -> Json<Value> {
    let mut ladder = st.ladder.lock().unwrap();
    let outcome = ladder.actuator().resume(&name);
    Json(json!({"name": name, "resumed": outcome.affected, "notes": outcome.notes}))
}

async fn suspended(State(st): State<AppState>) -> Json<Value> {
    let mut ladder = st.ladder.lock().unwrap();
    Json(json!(suspended_json(ladder.actuator())))
}

async fn watchlist(State(st): State<AppState>) -> Json<Value> {
    let det = st.det.read().unwrap();
    let mut out = Vec::new();
    for pattern in &st.cfg.watchlist {
        for s in det.matching(pattern) {
            out.push(json!({
                "name": s.name, "pid": s.pid, "status": s.state.to_string(),
                "verdict": s.verdict.as_str(), "pss_mb": mb(s.pss),
            }));
        }
    }
    Json(json!({"patterns": st.cfg.watchlist, "processes": out}))
}

async fn list_terminals() -> Json<Value> {
    let terms = extras::terminals();
    Json(json!({
        "summary": extras::terminal_summary(&terms),
        "idle": terms.iter().filter(|t| t.is_idle)
            .map(|t| json!({"pid": t.pid, "shell": t.shell, "reason": t.reason()}))
            .collect::<Vec<_>>(),
        "busy": terms.iter().filter(|t| !t.is_idle)
            .map(|t| json!({"pid": t.pid, "shell": t.shell,
                            "children": t.children, "reason": t.reason()}))
            .collect::<Vec<_>>(),
    }))
}

async fn close_idle_terminals() -> Json<Value> {
    let terms = extras::terminals();
    let closed = extras::close_idle_terminals(&terms);
    Json(json!({"closed": closed, "requested": terms.iter().filter(|t| t.is_idle).count()}))
}

async fn get_workspaces(State(st): State<AppState>) -> Json<Value> {
    match workspaces::layout() {
        Some(l) => Json(json!({
            "available": true,
            "n_workspaces": l.n_workspaces,
            "active_workspace": l.active_workspace,
            "windows": l.windows.iter().map(|w| json!({
                "xid": w.xid, "workspace": w.workspace, "pid": w.pid,
                "title": w.title, "wm_class": w.wm_class_app,
            })).collect::<Vec<_>>(),
            "wayland_native": l.wayland_native,
            "inert_rules": workspaces::inert_rules(&st.cfg.workspace_rules)
                .iter().map(|r| &r.pattern).collect::<Vec<_>>(),
        })),
        None => Json(json!({
            "available": false,
            "why": "wmctrl is unavailable — COSMIC exposes no workspace API",
        })),
    }
}

async fn sort_workspaces(State(st): State<AppState>) -> Json<Value> {
    let moved = workspaces::auto_sort(&st.cfg.workspace_rules);
    Json(json!({
        "moved": moved.iter().map(|m| json!({
            "title": m.title, "from": m.from, "to": m.to, "matched": m.matched,
        })).collect::<Vec<_>>(),
        "inert_rules": workspaces::inert_rules(&st.cfg.workspace_rules)
            .iter().map(|r| &r.pattern).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
struct AnalyzeBody {
    #[serde(default)]
    goal: String,
}

/// Collect tabs and report what should be reclaimed.
///
/// v1 called the Claude API here. v2 asks a local Nemotron, optionally escalates
/// to NVIDIA NIM, and falls back to the heuristic — which is the floor, not a
/// failure. The reply names the tier so the user can see which answered.
async fn analyze(State(st): State<AppState>, Json(body): Json<AnalyzeBody>) -> Json<Value> {
    let tabs = st.request_all_tabs().await;
    let p = st.pressure();
    let m = st.memory();

    // Take what the model tier needs and release the lock before awaiting: the
    // guard is not Send, and holding it over a network round trip would block
    // every other reader for its duration.
    let snap = {
        let det = st.det.read().unwrap();
        crate::ai::Snapshot::take(&det, &st.cfg.watchlist)
    };
    let decision = crate::ai::decide(
        &st.cfg,
        &st.ai,
        &snap,
        &tabs,
        crate::ai::MemoryState {
            psi_some: p.some_avg10,
            used_mb: mb(m.used()),
            total_mb: mb(m.total),
        },
        &body.goal,
    )
    .await;

    let by_id: std::collections::HashMap<i64, &Tab> = tabs.iter().map(|t| (t.id, t)).collect();
    let det = st.det.read().unwrap();

    Json(json!({
        "tier": decision.analysis.tier,
        "goal": body.goal,
        "summary": decision.summary,
        "tabs_reported": tabs.len(),
        "tabs_to_close": decision.tabs,
        "tab_detail": decision.tabs.iter().filter_map(|id| by_id.get(id)).take(25)
            .map(|t| json!({
                "id": t.id, "url": t.url, "title": t.title,
                "inactive_minutes": t.inactive_minutes,
            })).collect::<Vec<_>>(),
        "processes_to_suspend": decision.analysis.recommendation.processes_to_suspend,
        "idle_terminals_to_close": decision.analysis.recommendation.idle_terminals_to_close,
        // What the goal ranking judged off-topic. `null` means it could not judge:
        // no goal, no embedder, or scores too flat to mean anything.
        "irrelevant_to_goal": decision.irrelevant,
        // What a model asked for and was refused.
        "refused": {
            "processes": decision.analysis.dropped.processes,
            "tabs": decision.analysis.dropped.tabs,
        },
        "notes": decision.analysis.notes,
        "reclaimable_now": det.reclaimable(&st.cfg.watchlist)
            .iter().map(|s| &s.name).collect::<Vec<_>>(),
    }))
}

/// What the model tier can currently do, and what it cannot.
async fn ai_status(State(st): State<AppState>) -> Json<Value> {
    let local = match &st.ai.ollama {
        Some(o) => {
            let (ready, missing) = o.ready().await;
            json!({
                "configured": true, "ready": ready, "missing": missing,
                "model": o.model, "embed_model": o.embed_model,
            })
        }
        None => json!({"configured": false}),
    };

    let gpu = vram::primary().map(|g| json!({
        "name": g.name,
        "used_mb": mb(g.used), "total_mb": mb(g.total),
        "percent_used": (g.percent_used() * 10.0).round() / 10.0,
        "utilisation": g.utilisation,
        "room_for_local_model": g.has_room_for(vram::NEMOTRON_4B_BYTES, vram::MODEL_MARGIN),
    }));

    let page_out = {
        let mut ladder = st.ladder.lock().unwrap();
        let mut out = json!({
            "available": ladder.actuator().can_page_out(),
            "route": ladder.actuator().page_out_route(),
        });
        if let (Some(o), Value::Object(extra)) = (out.as_object_mut(), extras::helper()) {
            o.extend(extra);
        }
        out
    };

    Json(json!({
        "local": local,
        "cloud": st.ai.nim.as_ref().map(|n| json!({
            "configured": true,
            "enabled": st.cfg.nvidia.enabled,
            "model": n.model, "embed_model": n.embed_model,
            // Never the key itself.
            "key": n.key_fingerprint(),
        })).unwrap_or(json!({"configured": false})),
        "prefer": st.cfg.nvidia.prefer,
        "platform": st.platform.name,
        "gpu": gpu,
        "page_out": page_out,
    }))
}

#[derive(Deserialize)]
struct CloseTabsBody {
    #[serde(default)]
    tab_ids: Vec<i64>,
}

async fn close_tabs_route(
    State(st): State<AppState>,
    Json(body): Json<CloseTabsBody>,
) -> Json<Value> {
    let result = st.close_tabs(&body.tab_ids).await;
    Json(json!({
        "confirmed": result.confirmed,
        "queued": result.queued,
        "notes": result.notes,
    }))
}

async fn reclaim_scope(State(st): State<AppState>, Path(scope): Path<String>) -> impl IntoResponse {
    let Some(groups) = st.platform.ready_groups() else {
        let why = st.platform.groups.ready().err().map(|e| e.to_string()).unwrap_or_default();
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": format!("group reclaim unavailable: {why}")})),
        );
    };
    let Ok(group) = groups.group(&scope) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("no scope named {scope}")})),
        );
    };
    let ask = group.reclaimable().unwrap_or(0);
    let mut ladder = st.ladder.lock().unwrap();
    let outcome = ladder.actuator().reclaim_scope(group.as_ref(), ask);
    (
        StatusCode::OK,
        Json(json!({
            "scope": scope,
            "requested_mb": mb(ask),
            "freed_mb": mb(outcome.bytes_freed),
            "notes": outcome.notes,
        })),
    )
}

// ── Ladder ──────────────────────────────────────────────────────────────────

/// How the reuse predictor did on held-out data, against the LRU baseline,
/// and whether it is allowed to influence anything.
async fn predict_eval(State(st): State<AppState>) -> Json<Value> {
    let samples = st.history.lock().ok().and_then(|h| h.count_signals().ok());
    Json(json!({
        "metrics": st.predictor.metrics(),
        "behaviour_samples": samples,
        "min_train_examples": ramwarden_ml::jepa::MIN_TRAIN,
        "margin_over_lru": ramwarden_ml::jepa::MARGIN,
        // Scoring is always on the CPU; training goes to the GPU worker when
        // one is installed. Asking for a CUDA device here would create a
        // context in the daemon, which is exactly what the worker avoids.
        "scoring": "cpu",
        "training": if crate::worker::gpu_worker().is_some() { "gpu (ramwarden-gpu)" } else { "cpu" },
    }))
}

async fn predict_train(State(st): State<AppState>) -> Json<Value> {
    crate::predict::train_in_background(&st);
    Json(json!({"started": true, "see": "/predict/eval"}))
}

/// The card, who holds its memory, and which local models are loaded.
async fn gpu_report(State(st): State<AppState>) -> Json<Value> {
    let det = {
        // Clone the names out so the lock is not held across the awaits.
        let det = st.det.read().unwrap();
        ramwarden_core::detector::Detector::preloaded(det.is_warm(), det.snapshot().values().cloned().collect())
    };
    Json(crate::gpu::report(&det, &st.models).await)
}

#[derive(Deserialize, Default)]
struct UnloadBody {
    /// One model by name; every loaded model when absent.
    #[serde(default)]
    name: Option<String>,
}

async fn gpu_unload(State(st): State<AppState>, body: Option<Json<UnloadBody>>) -> Json<Value> {
    let want = body.and_then(|b| b.0.name);
    let mut results = Vec::new();
    for m in st.models.loaded().await {
        if want.as_ref().is_some_and(|w| *w != m.name) {
            continue;
        }
        let r = st.models.unload(&m).await;
        results.push(json!({"host": m.host, "name": m.name, "ok": r.is_ok(),
                            "error": r.err().map(|e| e.to_string())}));
    }
    Json(json!({"results": results}))
}

#[derive(Deserialize)]
struct DebloatBody {
    /// A profile name, a tweak id, or (for revert) `all`.
    name: String,
    #[serde(default)]
    include_optional: bool,
}

/// Every shipped tweak and its state on this machine.
async fn debloat_status() -> Json<Value> {
    Json(tokio::task::spawn_blocking(extras::debloat::status).await.unwrap_or_default())
}

async fn debloat_apply(Json(body): Json<DebloatBody>) -> Json<Value> {
    Json(
        tokio::task::spawn_blocking(move || extras::debloat::apply(&body.name, body.include_optional))
            .await
            .unwrap_or_default(),
    )
}

async fn debloat_revert(Json(body): Json<DebloatBody>) -> Json<Value> {
    Json(
        tokio::task::spawn_blocking(move || extras::debloat::revert(&body.name))
            .await
            .unwrap_or_default(),
    )
}

/// What the Tidy rung is tracking: quiet apps, and whether each has been
/// hinted as background and trimmed in its current quiet period.
async fn tidy(State(st): State<AppState>) -> Json<Value> {
    let det = st.det.read().unwrap();
    let ladder = st.ladder.lock().unwrap();
    let entries = ladder.tidy_entries(&det);
    Json(json!({
        "enabled": st.cfg.tidy.enabled,
        "after_minutes": st.cfg.tidy.after_minutes,
        "background_hints": st.cfg.tidy.background_hints && st.platform.background.available(),
        "trim": st.cfg.tidy.trim,
        "tracking": entries.iter().map(|e| json!({
            "pid": e.pid, "name": e.name,
            "quiet_minutes": (e.quiet_minutes * 10.0).round() / 10.0,
            "hinted": e.hinted, "trimmed": e.trimmed,
            "pss_mb": det.get(e.pid).map(|s| mb(s.pss)).unwrap_or(0.0),
        })).collect::<Vec<_>>(),
    }))
}

async fn ladder_plan(State(st): State<AppState>) -> Json<Value> {
    let det = st.det.read().unwrap();
    let p = st.pressure();

    let w = ladder::World {
        det: &det,
        groups: st.platform.ready_groups(),
        psi: p,
        available_bytes: ladder::available_bytes(st.platform.memory.as_ref()),
        watchlist: &st.cfg.watchlist,
        goal: String::new(),
        tabs: None,
        judge: None,
    };
    let ladder = st.ladder.lock().unwrap();
    let plan = ladder.plan(&w);

    Json(json!({
        "rung": plan.rung.map(|r| r.as_str()),
        "blocked": plan.blocked,
        "reclaim": plan.reclaim.iter().map(|(n, b)| json!({"scope": n, "ask_mb": mb(*b)}))
            .collect::<Vec<_>>(),
        "soft_cap": plan.soft_cap.as_ref().map(|(n, b)| json!({"scope": n, "cap_mb": mb(*b)})),
        "suspend": plan.suspend,
        "kill": plan.kill,
        "kill_pending": ladder.pending_kill().map(|p| json!({
            "targets": p.targets,
            "seconds_remaining": p.remaining().as_secs(),
        })),
        "kill_queue": ladder.queued_kills(),
        "undo": ladder.undo_stack(),
    }))
}

/// Resume whatever the ladder suspended most recently.
async fn undo_last(State(st): State<AppState>) -> impl IntoResponse {
    let undone = st.ladder.lock().unwrap().undo_last();
    match undone {
        Some((name, outcome)) => {
            tracing::info!("undo: resumed {name}");
            (
                StatusCode::OK,
                Json(json!({"resumed": name, "notes": outcome.notes})),
            )
        }
        None => (
            StatusCode::CONFLICT,
            Json(json!({"resumed": null, "notes": ["nothing to undo"]})),
        ),
    }
}

/// Applications from least to most recently used — the order the ladder
/// considers them in.
async fn lru(State(st): State<AppState>) -> Json<Value> {
    let det = st.det.read().unwrap();
    Json(json!({
        "coldest_first": det.coldest().map(|(name, r)| json!({
            "name": name,
            "idle_minutes": (r.idle().as_secs_f64() / 60.0).round(),
            "touched": r.touched,
        })).collect::<Vec<_>>(),
    }))
}

async fn cancel_kill(State(st): State<AppState>) -> Json<Value> {
    let cancelled = st.ladder.lock().unwrap().cancel_kill();
    if cancelled {
        tracing::warn!("pending kill cancelled by request");
    }
    Json(json!({"cancelled": cancelled}))
}

// ── History ─────────────────────────────────────────────────────────────────

/// Pause freezing for `secs` seconds (max 600) and thaw what is frozen, so a
/// window arranger can talk to every app. `DELETE /hold` ends it early.
async fn hold(State(st): State<AppState>, Path(secs): Path<u64>) -> Json<Value> {
    st.set_hold(secs);
    // Thaw now rather than waiting for the monitor's next tick.
    let thawed: Vec<String> = st.ladder.lock().unwrap().thaw_all().into_iter().map(|(n, _)| n).collect();
    tracing::info!("hold {secs}s; thawed {} group(s)", thawed.len());
    Json(json!({"hold_seconds": secs.min(600), "thawed": thawed}))
}

async fn release_hold(State(st): State<AppState>) -> Json<Value> {
    st.set_hold(0);
    Json(json!({"hold_seconds": 0}))
}

/// The Jev decision tier: whether it is on, what it has cost, and whether it is paused.
async fn intel() -> Json<Value> {
    Json(match crate::tabintel::global() {
        Some(i) => i.status(std::time::Instant::now()),
        None => json!({"enabled": false, "configured": false}),
    })
}

/// How often unloaded tabs are returned to: the data that calibrates the model.
async fn outcomes(State(st): State<AppState>) -> Json<Value> {
    let h = st.history.lock().unwrap();
    let now = ramwarden_core::history::unix_now() as i64;
    let day = h.labelled_outcomes(86_400, now).unwrap_or_default();
    let returned = day.iter().filter(|o| o.returned).count();
    let (mut predicted, mut brier) = (0.0, 0.0);
    for o in &day {
        predicted += o.p_return;
        brier += (o.p_return - if o.returned { 1.0 } else { 0.0 }).powi(2);
    }
    let n = day.len().max(1) as f64;
    Json(json!({
        "recorded": h.count_outcomes().unwrap_or(0),
        "labelled_24h": day.len(),
        "returned_24h": returned,
        // Calibration: if the model is right, these two roughly agree.
        "mean_predicted_return": if day.is_empty() { Value::Null } else { json!(predicted / n) },
        "actual_return_rate": if day.is_empty() { Value::Null } else { json!(returned as f64 / n) },
        // 0 is perfect; always answering 0.5 scores 0.25.
        "brier": if day.is_empty() { Value::Null } else { json!(brier / n) },
    }))
}

async fn history(State(st): State<AppState>) -> Json<Value> {
    let h = st.history.lock().unwrap();
    let tabs = h.recent_tabs(100).unwrap_or_default();
    Json(json!({
        "count": h.count_tabs().unwrap_or(0),
        "closed_tabs": tabs.iter().map(|t| json!({
            "id": t.id, "url": t.url, "title": t.title, "closed_at": t.closed_at,
            "ram_freed_mb": t.ram_freed_mb, "trigger_type": t.trigger_type,
            "goal_context": t.goal_context,
        })).collect::<Vec<_>>(),
    }))
}

async fn clear_history(State(st): State<AppState>) -> Json<Value> {
    let h = st.history.lock().unwrap();
    Json(json!({"deleted": h.clear_tabs().unwrap_or(0)}))
}

/// What the ladder has been doing unattended.
async fn actions(State(st): State<AppState>) -> Json<Value> {
    let h = st.history.lock().unwrap();
    let rows = h.recent_actions(100).unwrap_or_default();
    Json(json!({
        "total_reclaimed_mb": mb(h.total_freed("reclaim").unwrap_or(0) as u64),
        "actions": rows.iter().map(|a| json!({
            "at": a.at, "rung": a.rung, "trigger": a.trigger,
            "psi_some": a.psi_some, "psi_full": a.psi_full,
            "action": a.action, "target": a.target,
            "freed_mb": mb(a.bytes_freed.max(0) as u64),
            "succeeded": a.succeeded, "notes": a.notes,
        })).collect::<Vec<_>>(),
    }))
}

/// Training rows from the behaviour log.
///
/// Features plus a `returned` label, which is the thing worth predicting: not
/// "is this idle now" but "will the user want it back". Rows too recent to have
/// been observed for a full horizon are excluded rather than labelled false.
async fn signals(State(st): State<AppState>) -> Json<Value> {
    let h = st.history.lock().unwrap();
    let horizon = st.cfg.logging.horizon_seconds;
    let rows = h.return_labels(horizon, 0.5).unwrap_or_default();
    let returned = rows.iter().filter(|r| r.returned).count();

    Json(json!({
        "enabled": st.cfg.logging.enabled,
        "interval_seconds": st.cfg.logging.interval_seconds,
        "horizon_seconds": horizon,
        "samples_logged": h.count_signals().unwrap_or(0),
        "labelled_rows": rows.len(),
        // A set that is all one class teaches nothing; the balance is worth
        // seeing before anyone trains on it.
        "returned": returned,
        "not_returned": rows.len() - returned,
        "rows": rows.iter().rev().take(500).map(|r| json!({
            "epoch": r.epoch,
            "name": r.sample.name,
            "role": r.sample.role,
            "verdict": r.sample.verdict,
            "protection": r.sample.protection,
            "pss": r.sample.pss,
            "rss": r.sample.rss,
            "cpu_recent": r.sample.cpu_recent,
            "age_minutes": r.sample.age_minutes,
            "established": r.sample.established,
            "listening": r.sample.listening,
            "focused": r.sample.focused,
            "windowed": r.sample.windowed,
            "audio": r.sample.audio,
            "tty": r.sample.tty,
            "descendant": r.sample.descendant,
            "psi_some": r.context.psi_some,
            "psi_full": r.context.psi_full,
            "available": r.context.available,
            "returned": r.returned,
        })).collect::<Vec<_>>(),
    }))
}

// ── The window's state, in one call ─────────────────────────────────────────

/// Everything v1's `_window_state()` returned, so the GTK window can run out of
/// process against this daemon without being modified.
async fn window_state(State(st): State<AppState>) -> Json<Value> {
    let m = st.memory();
    let z = st.platform.memory.compression().unwrap_or_default();
    let p = st.pressure();
    let det = st.det.read().unwrap();
    let totals = det.totals();

    let mut procs: Vec<_> = det.snapshot().values().collect();
    procs.sort_by_key(|s| std::cmp::Reverse(s.pss));

    let suspended: Vec<Value> = {
        let mut ladder = st.ladder.lock().unwrap();
        suspended_json(ladder.actuator())
    };

    let hub = st.hub.lock().unwrap();
    Json(json!({
        "percent": m.percent(),
        "used_mb": mb(m.used()),
        "total_mb": mb(m.total),
        "warn_percent": st.cfg.thresholds.ram_percent,
        "browsers_connected": hub.reg.browsers_connected(),
        "processes": procs.iter().filter(|s| s.pss >= 80 * 1024 * 1024).take(40)
            .map(|s| signal_json(s)).collect::<Vec<_>>(),
        "totals_mb": {
            "PROTECTED": mb(totals["PROTECTED"]),
            "IN_USE": mb(totals["IN_USE"]),
            "IDLE": mb(totals["IDLE"]),
        },
        "suspended": suspended,
        // v2 additions the window can ignore.
        "psi_some": p.some_avg10,
        "psi_full": p.full_avg10,
        "pressure_estimated": p.estimated,
        "zram_saved_mb": mb(z.saved()),
        "warm": det.is_warm(),
    }))
}

/// What RamWarden has frozen, as `/suspended` and `/state` report it.
fn suspended_json(act: &mut ramwarden_core::actuator::Actuator) -> Vec<Value> {
    let control = act.control();
    act.suspended()
        .iter()
        .map(|e| {
            json!({
                "name": e.name,
                "pids": e.live_pids(),
                "rss_mb": mb(e.pss(control.as_ref())),
                "minutes": (e.minutes() * 10.0).round() / 10.0,
            })
        })
        .collect()
}

fn browser_snapshot(st: &AppState) -> Value {
    let hub = st.hub.lock().unwrap();
    let rows = crate::browser::snapshot(&hub.reg, st.cfg.thresholds.inactivity_minutes as i64);
    json!({"browsers_connected":hub.reg.browsers_connected(), "candidates":rows.iter().filter(|r|r.eligible).count(),
        "batch_limit":crate::browser::BATCH_LIMIT, "tabs":rows,
        "savings_note":"Per-tab memory is not measured. Unloading keeps tabs open; they reload when selected."})
}
async fn browser_tabs(State(st): State<AppState>) -> Json<Value> {
    // Older extensions only report when asked; don't let their visible list expire.
    { let hub=st.hub.lock().unwrap();
      for browser in hub.reg.socket_ids() { hub.send(&browser,&json!({"action":"get_tabs"})); }
    }
    Json(browser_snapshot(&st))
}
async fn browser_analyze(State(st): State<AppState>) -> Json<Value> {
    st.request_all_tabs().await;
    Json(browser_snapshot(&st))
}
#[derive(Deserialize)]
struct DiscardBody { targets: Vec<crate::browser::Target> }
async fn browser_discard(State(st): State<AppState>, Json(body): Json<DiscardBody>) -> Json<crate::hub::DiscardResult> {
    // A fresh socket report can veto a stale selection before dispatch.
    st.request_all_tabs().await;
    let result = st.discard_tabs(&body.targets).await;
    let notes = format!("{} unloaded, {} queued (not confirmed), {} refused; tabs remain open",
        result.confirmed.len(),result.queued.len(),result.refused.len());
    tracing::info!("browser unload: {notes}");
    if let Ok(history) = st.history.lock() {
        let _ = history.log(&ramwarden_core::history::NewAction {
            rung: -1, trigger: "manual".into(), action: "discard_tab".into(), target: "selected browser tabs".into(),
            succeeded: !result.confirmed.is_empty(), notes, ..Default::default()
        });
    }
    Json(result)
}

async fn browser_close(State(st): State<AppState>, Json(body): Json<DiscardBody>) -> Json<crate::hub::DiscardResult> {
    let requested_at=std::time::Instant::now();
    st.request_all_tabs().await;
    let (fresh, stale): (Vec<_>,Vec<_>) = {
        let hub=st.hub.lock().unwrap();
        body.targets.into_iter().partition(|t| !hub.reg.is_socket(&t.browser) || hub.reg.reported_since(&t.browser,requested_at))
    };
    let mut result=st.close_selected_tabs(&fresh).await;
    result.refused.extend(stale);
    tracing::info!(confirmed=result.confirmed.len(),queued=result.queued.len(),refused=result.refused.len(),"manual tab close completed");
    Json(result)
}
