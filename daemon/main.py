"""
RamWarden daemon — FastAPI server + RAM monitoring loop.
Run with: python3 -m daemon.main
"""
import asyncio
import json
import logging
import threading
import time
import uuid
from contextlib import asynccontextmanager
from dataclasses import asdict

import uvicorn
from fastapi import FastAPI, WebSocket, WebSocketDisconnect
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel

from . import config, history, monitor
from .analyzer import analyze, _claude_analyze
from .browser_windows import detect_private_windows, close_private_window
from .process_manager import ProcessManager
from .terminal_manager import list_terminals, close_idle_terminals
from . import workspace_manager as wm
from .workspace_manager import start_workspace_watcher, stop_workspace_watcher
from .process_profiler import get_profiler

logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s")
log = logging.getLogger("ramwarden")

# Suppress "Invalid HTTP request received" noise that Firefox generates when
# its extension context probes the WebSocket port with a non-WS request.
class _SuppressWSNoise(logging.Filter):
    def filter(self, record):
        return "Invalid HTTP request received" not in record.getMessage()

logging.getLogger("uvicorn.error").addFilter(_SuppressWSNoise())

# Multi-browser WebSocket support (Chrome/Brave):
_connections: dict[str, WebSocket] = {}          # conn_id → WebSocket
_pending_futures: dict[str, asyncio.Future] = {}  # conn_id → Future[list[tab]]
_ws_last_tabs: dict[str, list[dict]] = {}        # conn_id → last tab report (cached for /debug/tabs)

# HTTP polling support (Firefox and any browser that can't use WebSocket):
# Maps stable browser_id → latest tab snapshot + queued close commands
_poll_browsers: dict[str, list[dict]] = {}         # browser_id → tab list
_poll_pending:  dict[str, list[dict]] = {}         # browser_id → [{action,tabIds}]

_tab_owners: dict[int, str] = {}                   # tab_id → conn_id or browser_id

_analyze_lock = asyncio.Lock()
_loop: asyncio.AbstractEventLoop | None = None

# When RAM threshold fires (e.g. at daemon startup), HTTP-poll browsers (Firefox)
# may not have polled yet. Wait this long for at least one to check in before
# running the analysis without their tabs.
BROWSER_CONNECT_GRACE_S = 3.0   # normal grace when re-analysis triggered mid-session
STARTUP_GRACE_S         = 15.0  # longer grace at daemon startup (Firefox event page is slow to wake)
STARTUP_WINDOW_S        = 30.0  # treat first N seconds as "startup"

_daemon_start = time.monotonic()


@asynccontextmanager
async def lifespan(app: FastAPI):
    global _loop
    cfg = config.load()
    history.init_db()
    _loop = asyncio.get_event_loop()

    mon = monitor.Monitor(on_threshold=_threshold_callback, poll_interval=10.0)
    t = threading.Thread(target=mon.start, daemon=True)
    t.start()
    log.info("RAM monitor started (threshold=%.0f%%)", cfg.thresholds.ram_percent)

    # Workspace watcher: continuously moves XWayland windows to their assigned workspaces
    if cfg.workspace_rules:
        start_workspace_watcher(cfg.workspace_rules, interval_s=15.0)

    yield
    stop_workspace_watcher()
    mon.stop()


app = FastAPI(title="RamWarden", lifespan=lifespan)
app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_methods=["*"],
    allow_headers=["*"],
)


# ── WebSocket (browser extensions) ───────────────────────────────────────────

@app.websocket("/ws")
async def ws_endpoint(websocket: WebSocket):
    await websocket.accept()
    conn_id = uuid.uuid4().hex[:8]
    _connections[conn_id] = websocket
    log.info("Extension connected [%s] — %d browser(s) connected", conn_id, len(_connections))
    try:
        while True:
            raw = await websocket.receive_text()
            msg = json.loads(raw)
            await _handle_msg(msg, conn_id)
    except WebSocketDisconnect:
        log.info("Extension disconnected [%s]", conn_id)
    finally:
        _connections.pop(conn_id, None)
        # Clean up tab ownership for this connection
        stale = [tid for tid, cid in list(_tab_owners.items()) if cid == conn_id]
        for tid in stale:
            _tab_owners.pop(tid, None)
        fut = _pending_futures.pop(conn_id, None)
        if fut and not fut.done():
            fut.set_result([])


async def _handle_msg(msg: dict, conn_id: str):
    action = msg.get("action")

    if action == "tab_report":
        tabs = msg.get("tabs", [])
        _ws_last_tabs[conn_id] = tabs  # cache for /debug/tabs
        for tab in tabs:
            _tab_owners[tab["id"]] = conn_id
        fut = _pending_futures.get(conn_id)
        if fut and not fut.done():
            fut.set_result(tabs)

    elif action == "tabs_closed":
        log.info("Extension [%s] confirmed closed tab IDs: %s", conn_id, msg.get("tabIds"))

    elif action == "pong":
        pass


async def _request_all_tabs(timeout: float = 6.0) -> list[dict]:
    """Ask every connected browser extension for its tabs; merges results."""
    all_tabs: list[dict] = []

    # If no HTTP-poll browser has checked in yet, wait for one.
    # Use a longer grace during startup because Firefox's event page can take 10+ seconds
    # to wake, seed tabs, and post its first poll after a daemon restart.
    if not _poll_browsers:
        since_start = time.monotonic() - _daemon_start
        grace = STARTUP_GRACE_S if since_start < STARTUP_WINDOW_S else BROWSER_CONNECT_GRACE_S
        waited = 0.0
        step   = 0.2
        while waited < grace and not _poll_browsers:
            await asyncio.sleep(step)
            waited += step
        if _poll_browsers:
            log.info("Poll browser(s) arrived after %.1fs grace wait", waited)
        else:
            log.info("No poll browsers after %.1fs grace — proceeding without them", waited)

    # ── WebSocket browsers (Chrome/Brave) ────────────────────────────────────
    if _connections:
        loop = asyncio.get_event_loop()
        futs = {}
        for conn_id, ws in list(_connections.items()):
            fut = loop.create_future()
            _pending_futures[conn_id] = fut
            try:
                await ws.send_text(json.dumps({"action": "get_tabs"}))
                futs[conn_id] = fut
            except Exception as e:
                log.warning("Tab request failed for [%s]: %s", conn_id, e)
                fut.set_result([])

        for conn_id, fut in futs.items():
            try:
                tabs = await asyncio.wait_for(asyncio.shield(fut), timeout=timeout)
                all_tabs.extend(tabs)
                log.info("Extension [%s] reported %d tabs (WS)", conn_id, len(tabs))
            except asyncio.TimeoutError:
                log.warning("Tab report timed out for [%s]", conn_id)
            finally:
                _pending_futures.pop(conn_id, None)

    # ── HTTP polling browsers (Firefox) ──────────────────────────────────────
    for browser_id, tabs in _poll_browsers.items():
        all_tabs.extend(tabs)
        log.info("Browser [%s] contributed %d tabs (HTTP poll)", browser_id[:8], len(tabs))

    return all_tabs


async def _close_tabs(tab_ids: list[int]):
    """Route close commands to the correct browser by tab ownership."""
    by_owner: dict[str, list[int]] = {}
    unowned = []
    for tid in tab_ids:
        cid = _tab_owners.get(tid)
        if cid:
            by_owner.setdefault(cid, []).append(tid)
        else:
            unowned.append(tid)

    if unowned:
        log.warning("No browser found for tab IDs %s — skipping", unowned)

    for cid, ids in by_owner.items():
        if cid in _connections:
            # WebSocket browser
            ws = _connections[cid]
            try:
                await ws.send_text(json.dumps({"action": "close", "tabIds": ids}))
                log.info("Sent close for %d tab(s) to WS extension [%s]", len(ids), cid)
            except Exception as e:
                log.warning("Close send failed for [%s]: %s", cid, e)
        elif cid in _poll_browsers:
            # HTTP polling browser — queue the command; picked up on next poll
            _poll_pending.setdefault(cid, []).append({"action": "close", "tabIds": ids})
            log.info("Queued close for %d tab(s) to poll browser [%s]", len(ids), cid[:8])


# ── Analysis flow ─────────────────────────────────────────────────────────────

async def _run_analysis(trigger_type: str = "auto"):
    async with _analyze_lock:
        snap = monitor.snapshot()
        tabs = await _request_all_tabs()
        terminals = await asyncio.to_thread(list_terminals)
        layout = await asyncio.to_thread(wm.get_layout)
        private_windows = await asyncio.to_thread(detect_private_windows)

        force_claude = (trigger_type == "manual")
        recommendation = await asyncio.to_thread(analyze, snap, tabs, terminals, layout, force_claude)

        has_work = (
            recommendation.tabs_to_close
            or recommendation.processes_to_suspend
            or recommendation.idle_terminals_to_close
            or recommendation.workspaces_to_sort
            or private_windows
        )
        if not has_work:
            log.info("Analysis complete — nothing to do")
            return

        cfg = config.get()

        # Closure the dialog can call to re-run Claude with a user goal
        def re_analyze_fn(goal: str):
            return _claude_analyze(snap, tabs, cfg, terminals, layout, goal_context=goal)

        confirmed, selected_tab_ids, private_xids, goal_text = await asyncio.to_thread(
            _show_prompt, recommendation, snap, terminals, tabs, private_windows, re_analyze_fn
        )
        if not confirmed:
            log.info("User skipped RAM cleanup")
            return

        # Close browser tabs
        if selected_tab_ids:
            closed_tabs = [t for t in tabs if t["id"] in selected_tab_ids]
            await _close_tabs(selected_tab_ids)
            history.save_tabs(
                [{"url": t["url"], "title": t["title"]} for t in closed_tabs],
                ram_freed_mb=recommendation.estimated_ram_freed_mb,
                trigger_type=trigger_type,
                goal_context=goal_text,
            )
            log.info("Closed %d tab(s) (goal: %r)", len(closed_tabs), goal_text or "none")

        # Close private/incognito windows
        for xid in private_xids:
            ok = await asyncio.to_thread(close_private_window, xid)
            log.info("Closed private window %s: %s", xid, "ok" if ok else "failed")

        # Suspend processes
        pm = ProcessManager()
        for proc_name in recommendation.processes_to_suspend:
            pm.suspend(proc_name)
            log.info("Suspended process: %s", proc_name)

        # Close idle terminals
        if recommendation.idle_terminals_to_close:
            idle_map = {t.pid: t for t in terminals if t.is_idle}
            targets = [idle_map[pid] for pid in recommendation.idle_terminals_to_close if pid in idle_map]
            closed = close_idle_terminals(targets)
            log.info("Closed %d idle terminal(s)", len(closed))

        # Sort workspaces
        if recommendation.workspaces_to_sort:
            moved = wm.auto_sort(cfg.workspace_rules)
            log.info("Sorted %d window(s) into workspaces", len(moved))


def _show_prompt(
    recommendation, snap, terminals=None,
    tabs=None, private_windows=None, re_analyze_fn=None,
) -> tuple[bool, list[int], list[str], str]:
    from ui.prompt import show_prompt
    return show_prompt(recommendation, snap, terminals, tabs, private_windows, re_analyze_fn)


def _threshold_callback(snap: monitor.RAMSnapshot):
    if _loop is None:
        return
    asyncio.run_coroutine_threadsafe(_run_analysis("auto"), _loop)


# ── REST endpoints ────────────────────────────────────────────────────────────

class TabPollBody(BaseModel):
    browser_id: str
    tabs: list[dict]


@app.post("/api/tabs")
async def poll_tabs(body: TabPollBody):
    """
    Firefox (and any HTTP-polling browser) calls this every 30s.
    - Body: { browser_id, tabs: [...] }
    - Response: { commands: [{action, tabIds}] }
    The daemon queues close commands here; browser executes them on next response.
    """
    bid = body.browser_id
    _poll_browsers[bid] = body.tabs
    for tab in body.tabs:
        _tab_owners[tab["id"]] = bid

    pending = _poll_pending.pop(bid, [])
    if pending:
        log.info("Delivering %d queued command(s) to poll browser [%s]", len(pending), bid[:8])
    return {"commands": pending, "browser_id": bid}


@app.get("/health")
def health():
    return {"ok": True}


@app.get("/debug/tabs")
def debug_tabs():
    """Show what each connected browser last reported."""
    ws = {}
    for cid in _connections:
        last = _ws_last_tabs.get(cid, [])
        incognito_count = sum(1 for t in last if t.get("incognito"))
        ws[cid] = {
            "tab_count": len(last),
            "incognito_count": incognito_count,
            "sample": last[:3],
        }
    return {
        "ws_browsers": ws,
        "poll_browsers": {
            bid: {"tab_count": len(tabs), "sample": tabs[:3]}
            for bid, tabs in _poll_browsers.items()
        },
    }


@app.get("/stats")
def get_stats():
    snap = monitor.snapshot()
    terminals = list_terminals()
    private_windows = detect_private_windows()
    return {
        "percent": snap.percent,
        "used_mb": snap.used_mb,
        "total_mb": snap.total_mb,
        "processes": [asdict(p) for p in snap.processes],
        "browsers_connected": len(_connections) + len(_poll_browsers),
        "ws_browsers": len(_connections),
        "poll_browsers": len(_poll_browsers),
        "private_windows": [
            {"type": w.type, "title": w.title, "rss_mb": w.rss_mb, "closeable": w.closeable}
            for w in private_windows
        ],
        "terminals": {
            "idle": [{"pid": t.pid, "shell": t.shell_name} for t in terminals if t.is_idle],
            "busy": [{"pid": t.pid, "shell": t.shell_name, "children": t.child_processes[:5]} for t in terminals if not t.is_idle],
        },
    }


@app.get("/ram-report")
def ram_report():
    """Human-readable breakdown: what's using RAM and what can be freed."""
    import psutil
    vm = psutil.virtual_memory()
    profiler = get_profiler()
    report = profiler.report()

    # Summarise by category
    by_cat = report["by_category"]
    summary = {}
    for cat, procs in by_cat.items():
        total_mb = sum(p["rss_mb"] for p in procs)
        summary[cat] = {
            "total_mb": round(total_mb, 1),
            "count": len(procs),
            "top": sorted(procs, key=lambda p: p["rss_mb"], reverse=True)[:5],
        }

    return {
        "ram": {
            "total_gb": round(vm.total / 1024**3, 1),
            "used_gb": round(vm.used / 1024**3, 1),
            "available_gb": round(vm.available / 1024**3, 1),
            "percent": vm.percent,
            "buffers_cache_gb": round((vm.buffers + vm.cached) / 1024**3, 1),
        },
        "by_category": summary,
        "suspend_candidates": report["candidates"],
        "interpretation": _interpret(vm, summary, report["candidates"]),
    }


def _interpret(vm, summary, candidates) -> str:
    lines = []
    pct = vm.percent
    avail = vm.available / 1024**3
    bc = (vm.buffers + vm.cached) / 1024**3

    lines.append(f"RAM: {pct:.0f}% used, {avail:.1f} GB truly available.")
    lines.append(f"Linux holds {bc:.1f} GB as disk cache — it frees automatically when needed.")

    browser_mb = summary.get("BROWSER", {}).get("total_mb", 0)
    if browser_mb > 500:
        lines.append(f"Browser renderers: {browser_mb:.0f} MB — close idle tabs to reclaim this.")

    work_mb = sum(summary.get(c, {}).get("total_mb", 0) for c in ("WORK_TOOL", "AGENT"))
    if work_mb > 200:
        lines.append(f"Work tools + agents: {work_mb:.0f} MB — intentional, leaving alone.")

    if candidates:
        cand_mb = sum(c["rss_mb"] for c in candidates)
        names = ", ".join(c["name"] for c in candidates[:4])
        lines.append(f"Suspend candidates: {names} ({cand_mb:.0f} MB total idle).")

    return " ".join(lines)


@app.get("/terminals")
def get_terminals():
    terminals = list_terminals()
    return {
        "idle": [asdict(t) for t in terminals if t.is_idle],
        "busy": [asdict(t) for t in terminals if not t.is_idle],
    }


@app.post("/terminals/close-idle")
async def close_idle():
    terminals = list_terminals()
    idle = [t for t in terminals if t.is_idle]
    closed = await asyncio.to_thread(close_idle_terminals, idle)
    return {"closed_pids": closed, "count": len(closed)}


@app.get("/workspaces")
def get_workspaces():
    layout = wm.get_layout()
    if layout is None:
        return {"ok": False, "error": "Wnck unavailable (headless or Wayland without XWayland?)"}
    return {
        "ok": True,
        "n_workspaces": layout.n_workspaces,
        "active_workspace": layout.active_workspace,
        "windows": [
            {"xid": w.xid, "name": w.name, "class": w.wm_class, "workspace": w.workspace, "pid": w.pid}
            for w in layout.windows
        ],
    }


@app.post("/workspaces/sort")
async def sort_workspaces():
    cfg = config.get()
    if not cfg.workspace_rules:
        return {"ok": False, "error": "No workspace rules configured in ramwarden.toml"}
    moved = await asyncio.to_thread(wm.auto_sort, cfg.workspace_rules)
    return {"ok": True, "moved": moved, "count": len(moved)}


@app.post("/resume/{name}")
async def resume_process(name: str):
    """SIGCONT a suspended process by name. Use this to unfreeze anything RamWarden stopped."""
    pm = ProcessManager()
    resumed = pm.resume(name)
    if not resumed:
        return {"ok": False, "error": f"No stopped process found matching '{name}'"}
    return {"ok": True, "resumed_pids": resumed, "count": len(resumed)}


@app.get("/watchlist")
def get_watchlist():
    """Show current watchlist and the status of each process."""
    pm = ProcessManager()
    return {"processes": pm.watchlist_status()}


@app.post("/analyze")
async def trigger_analysis():
    asyncio.create_task(_run_analysis("manual"))
    return {"status": "analysis_started"}


@app.get("/history")
def get_history(limit: int = 50):
    return [
        {
            "id": t.id, "url": t.url, "title": t.title,
            "closed_at": t.closed_at, "ram_freed_mb": t.ram_freed_mb,
            "trigger_type": t.trigger_type,
        }
        for t in history.recent(limit)
    ]


@app.delete("/history")
def clear_history():
    history.clear()
    return {"status": "cleared"}


# ── Entry point ───────────────────────────────────────────────────────────────

def main():
    cfg = config.load()
    uvicorn.run(app, host=cfg.server.host, port=cfg.server.port, log_level="info")


if __name__ == "__main__":
    main()
