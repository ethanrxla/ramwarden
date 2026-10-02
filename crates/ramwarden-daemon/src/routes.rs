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
use ramwarden_core::{ladder, private, terminals, workspaces};
use ramwarden_ai::vram;
use ramwarden_kernel::{meminfo, psi, zram};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::hub::AppState;
use crate::tabs::{Tab, Transport};

pub fn router(state: AppState) -> Router {
    Router::new()
        // ── Contracts the shipped extensions depend on ──────────────────────
        .route("/api/tabs", post(poll_tabs))
        .route("/ws", get(crate::ws::handler))
        // ── v1 endpoints ────────────────────────────────────────────────────
        .route("/health", get(health))
        .route("/stats", get(stats))
        .route("/ram-report", get(ram_report))
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
        .route("/actions", get(actions))
        .route("/ai", get(ai_status))
        .route("/signals", get(signals))
        .route("/reclaim/{scope}", post(reclaim_scope))
        .route("/tabs/close", post(close_tabs_route))
        .route("/history/clear", delete(clear_history))
        .with_state(state)
}

fn mb(bytes: u64) -> f64 {
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
    let m = meminfo::read(&st.root).unwrap_or_default();
    let det = st.det.read().unwrap();
    let hub = st.hub.lock().unwrap();
    let terms = terminals::list(&st.root).unwrap_or_default();

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
        "private_windows": private::detect(&st.root, false).iter().map(|c| json!({
            "type": c.kind.as_str(), "title": c.title,
            "rss_mb": mb(c.pss), "closeable": c.closeable,
        })).collect::<Vec<_>>(),
        "terminals": {
            "idle": terms.iter().filter(|t| t.is_idle)
                .map(|t| json!({"pid": t.pid, "shell": t.shell})).collect::<Vec<_>>(),
            "busy": terms.iter().filter(|t| !t.is_idle)
                .map(|t| json!({"pid": t.pid, "shell": t.shell,
                                "children": t.children.iter().take(5).collect::<Vec<_>>()}))
                .collect::<Vec<_>>(),
        },
    }))
}

/// A breakdown of what memory is doing, and how far v1 would have been wrong.
async fn ram_report(State(st): State<AppState>) -> Json<Value> {
    let m = meminfo::read(&st.root).unwrap_or_default();
    let z = zram::total(&st.root).unwrap_or_default();
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
        "zram": {
            "stored_mb": mb(z.orig_data_size),
            "ram_cost_mb": mb(z.mem_used_total),
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
        })).collect::<Vec<_>>(),
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
    let p = psi::system_memory(&st.root).unwrap_or_default();
    let m = meminfo::read(&st.root).unwrap_or_default();
    let rung = ladder::rung_for(&st.cfg.ladder, &p, m.available);
    Json(json!({
        "some": {"avg10": p.some.avg10, "avg60": p.some.avg60, "avg300": p.some.avg300},
        "full": {"avg10": p.full.avg10, "avg60": p.full.avg60, "avg300": p.full.avg300},
        "available_mb": mb(m.available),
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
    let root = ladder.actuator().root().clone();
    let entries: Vec<Value> = ladder
        .actuator()
        .suspended()
        .iter()
        .map(|e| {
            json!({
                "name": e.name,
                "pids": e.live_pids(),
                "rss_mb": mb(e.pss(&root)),
                "minutes": (e.minutes() * 10.0).round() / 10.0,
            })
        })
        .collect();
    Json(json!(entries))
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

async fn list_terminals(State(st): State<AppState>) -> Json<Value> {
    let terms = terminals::list(&st.root).unwrap_or_default();
    Json(json!({
        "summary": terminals::summary(&terms),
        "idle": terms.iter().filter(|t| t.is_idle)
            .map(|t| json!({"pid": t.pid, "shell": t.shell, "reason": t.reason()}))
            .collect::<Vec<_>>(),
        "busy": terms.iter().filter(|t| !t.is_idle)
            .map(|t| json!({"pid": t.pid, "shell": t.shell,
                            "children": t.children, "reason": t.reason()}))
            .collect::<Vec<_>>(),
    }))
}

async fn close_idle_terminals(State(st): State<AppState>) -> Json<Value> {
    let terms = terminals::list(&st.root).unwrap_or_default();
    let closed = terminals::close_idle(&terms);
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
    let p = psi::system_memory(&st.root).unwrap_or_default();
    let m = meminfo::read(&st.root).unwrap_or_default();

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
            psi_some: p.some.avg10,
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
        json!({
            "available": ladder.actuator().can_page_out(),
            "route": ladder.actuator().page_out_route(),
            "helper_socket": ramwarden_core::helper::socket_path(),
            "helper_present": ramwarden_core::helper::present(),
        })
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
    let uid = unsafe { getuid() };
    let Ok(h) = ramwarden_kernel::cgroup::Hierarchy::user_session(&st.root, uid) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "no delegated cgroup memory controller"})),
        );
    };
    let Ok(s) = h.scope(&scope) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("no scope named {scope}")})),
        );
    };
    let ask = s.stat().map(|st| st.reclaimable()).unwrap_or(0);
    let mut ladder = st.ladder.lock().unwrap();
    let outcome = ladder.actuator().reclaim_scope(&s, ask);
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

async fn ladder_plan(State(st): State<AppState>) -> Json<Value> {
    let uid = unsafe { getuid() };
    let h = ramwarden_kernel::cgroup::Hierarchy::user_session(&st.root, uid).ok();
    let det = st.det.read().unwrap();
    let p = psi::system_memory(&st.root).unwrap_or_default();

    let w = ladder::World {
        det: &det,
        hierarchy: h.as_ref(),
        psi: p,
        available_bytes: ladder::available_bytes(&st.root),
        watchlist: &st.cfg.watchlist,
        goal: String::new(),
        tabs: None,
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
    let m = meminfo::read(&st.root).unwrap_or_default();
    let z = zram::total(&st.root).unwrap_or_default();
    let p = psi::system_memory(&st.root).unwrap_or_default();
    let det = st.det.read().unwrap();
    let totals = det.totals();

    let mut procs: Vec<_> = det.snapshot().values().collect();
    procs.sort_by_key(|s| std::cmp::Reverse(s.pss));

    let suspended: Vec<Value> = {
        let mut ladder = st.ladder.lock().unwrap();
        let root = ladder.actuator().root().clone();
        ladder
            .actuator()
            .suspended()
            .iter()
            .map(|e| {
                json!({
                    "name": e.name,
                    "pids": e.live_pids(),
                    "rss_mb": mb(e.pss(&root)),
                    "minutes": (e.minutes() * 10.0).round() / 10.0,
                })
            })
            .collect()
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
        "psi_some": p.some.avg10,
        "psi_full": p.full.avg10,
        "zram_saved_mb": mb(z.saved()),
        "warm": det.is_warm(),
    }))
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn getuid() -> u32;
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
