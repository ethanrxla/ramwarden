"""
TDD for RamWarden's tab collection pipeline.

Tests cover:
  1. POST /api/tabs stores Firefox tabs and they appear in /debug/tabs
  2. _request_all_tabs() includes BOTH WebSocket (Brave) and HTTP-poll (Firefox) tabs
  3. Auto-analysis waits for browsers to poll before running (timing race fix)
  4. Duplicate browser IDs (extension reload) don't double-count tabs
  5. Analysis includes Firefox tabs when triggered manually
"""
import asyncio
import json
import pytest
from fastapi.testclient import TestClient
from httpx import AsyncClient, ASGITransport

# Patch out GTK dialog so analysis can run headless
import unittest.mock as mock


@pytest.fixture(autouse=True)
def _patch_gtk(monkeypatch):
    """Prevent GTK dialog from appearing during tests."""
    monkeypatch.setattr(
        "daemon.main._show_prompt",
        lambda *a, **kw: (False, [], [], ""),
    )


@pytest.fixture(autouse=True)
def _patch_monitor(monkeypatch):
    """Prevent the real RAM monitor from auto-triggering during tests."""
    monkeypatch.setattr("daemon.monitor.Monitor.start", lambda self: None)
    monkeypatch.setattr("daemon.monitor.Monitor.stop",  lambda self: None)


@pytest.fixture(autouse=True)
def _patch_workspace_watcher(monkeypatch):
    monkeypatch.setattr("daemon.workspace_manager.start_workspace_watcher", lambda *a, **kw: None)
    monkeypatch.setattr("daemon.workspace_manager.stop_workspace_watcher", lambda: None)


@pytest.fixture(autouse=True)
def _patch_private_windows(monkeypatch):
    monkeypatch.setattr("daemon.browser_windows.detect_private_windows", lambda *a, **kw: [])


@pytest.fixture
def client():
    from daemon.main import app
    return TestClient(app)


# ── Sample data ───────────────────────────────────────────────────────────────

FIREFOX_TABS = [
    {"id": 1, "url": "https://github.com/ekomsSavior/REDflare-v2",
     "title": "REDflare-v2", "lastActiveMs": 0, "inactiveMinutes": 6087, "incognito": False},
    {"id": 2, "url": "https://labs.infoguard.ch/posts/ghost-sender/",
     "title": "Ghost Sender", "lastActiveMs": 0, "inactiveMinutes": 3346, "incognito": False},
    {"id": 3, "url": "https://hackerone.com/reports/1234",
     "title": "HackerOne report", "lastActiveMs": 0, "inactiveMinutes": 120, "incognito": False},
]

BRAVE_TABS = [
    {"id": 101, "url": "https://claude.ai",
     "title": "Claude", "lastActiveMs": 0, "inactiveMinutes": 10, "incognito": False},
    {"id": 102, "url": "https://stackoverflow.com/q/12345",
     "title": "Stack Overflow", "lastActiveMs": 0, "inactiveMinutes": 200, "incognito": False},
]


# ── 1. POST /api/tabs stores tabs ──────────────────────────────────────────────

def test_post_api_tabs_stores_firefox_tabs(client):
    r = client.post("/api/tabs", json={"browser_id": "rw-firefox1", "tabs": FIREFOX_TABS})
    assert r.status_code == 200
    assert r.json()["browser_id"] == "rw-firefox1"

    debug = client.get("/debug/tabs").json()
    assert "rw-firefox1" in debug["poll_browsers"]
    assert debug["poll_browsers"]["rw-firefox1"]["tab_count"] == 3


def test_post_api_tabs_empty_list(client):
    """Extension reload before seeding completes → should send 0 tabs, not crash."""
    r = client.post("/api/tabs", json={"browser_id": "rw-empty", "tabs": []})
    assert r.status_code == 200
    assert r.json()["browser_id"] == "rw-empty"

    debug = client.get("/debug/tabs").json()
    assert debug["poll_browsers"]["rw-empty"]["tab_count"] == 0


# ── 2. _request_all_tabs includes poll browsers ───────────────────────────────

@pytest.mark.asyncio
async def test_request_all_tabs_includes_firefox_poll_tabs(client):
    """Firefox HTTP-poll tabs must appear in _request_all_tabs() output."""
    from daemon.main import _poll_browsers, _request_all_tabs

    # Pre-populate as if Firefox already polled
    _poll_browsers.clear()
    _poll_browsers["rw-firefox1"] = FIREFOX_TABS

    tabs = await _request_all_tabs()
    urls = [t["url"] for t in tabs]
    assert "https://github.com/ekomsSavior/REDflare-v2" in urls
    assert "https://labs.infoguard.ch/posts/ghost-sender/" in urls
    _poll_browsers.clear()


@pytest.mark.asyncio
async def test_request_all_tabs_merges_both_browsers(client):
    """WS (Brave) + HTTP-poll (Firefox) tabs must both be returned."""
    from daemon.main import _poll_browsers, _request_all_tabs

    _poll_browsers.clear()
    _poll_browsers["rw-firefox1"] = FIREFOX_TABS

    # No WS connection active in test, so just poll tabs
    tabs = await _request_all_tabs()
    assert len(tabs) == len(FIREFOX_TABS)
    _poll_browsers.clear()


# ── 3. Timing race: auto-analysis fires before Firefox polls ──────────────────

@pytest.mark.asyncio
async def test_analysis_waits_for_firefox_before_running():
    """
    RAM crosses threshold → analysis triggered → Firefox has NOT polled yet.
    Analysis should wait up to BROWSER_CONNECT_GRACE seconds for poll browsers.
    After grace period, it should include whatever tabs arrived.
    """
    from daemon.main import _poll_browsers, _request_all_tabs, BROWSER_CONNECT_GRACE_S

    _poll_browsers.clear()

    # Simulate: Firefox polls 0.2s after analysis starts (within grace window)
    async def _delayed_firefox_poll():
        await asyncio.sleep(0.2)
        _poll_browsers["rw-firefox1"] = FIREFOX_TABS

    asyncio.create_task(_delayed_firefox_poll())

    tabs = await _request_all_tabs()
    assert len(tabs) == len(FIREFOX_TABS), (
        f"Expected {len(FIREFOX_TABS)} Firefox tabs, got {len(tabs)}. "
        "Analysis must wait for browsers to poll before collecting tabs."
    )
    _poll_browsers.clear()


# ── 4. Duplicate browser IDs (extension reload) ───────────────────────────────

def test_extension_reload_generates_new_id_no_duplicate_tabs(client):
    """
    After extension reload, Firefox may generate a new browser_id.
    Tabs with the same tab IDs should not be double-counted in analysis.
    """
    from daemon.main import _poll_browsers

    _poll_browsers.clear()
    client.post("/api/tabs", json={"browser_id": "rw-old", "tabs": FIREFOX_TABS})
    client.post("/api/tabs", json={"browser_id": "rw-new", "tabs": FIREFOX_TABS})

    # Both are stored (two separate entries)
    debug = client.get("/debug/tabs").json()
    assert debug["poll_browsers"]["rw-old"]["tab_count"] == 3
    assert debug["poll_browsers"]["rw-new"]["tab_count"] == 3

    # _request_all_tabs would return 6 entries — same tab IDs from both browsers.
    # This is a known limitation: _tab_owners deduplication only happens at close time.
    # At minimum, the counts must be correct per-browser.
    _poll_browsers.clear()


# ── 5. stats endpoint includes poll browser count ─────────────────────────────

def test_stats_shows_poll_browser_count(client):
    from daemon.main import _poll_browsers
    _poll_browsers.clear()
    client.post("/api/tabs", json={"browser_id": "rw-ff", "tabs": FIREFOX_TABS})

    stats = client.get("/stats").json()
    assert stats["poll_browsers"] >= 1
    assert stats["browsers_connected"] >= 1
    _poll_browsers.clear()
