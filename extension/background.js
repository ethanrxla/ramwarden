const DEFAULT_HOST = "localhost:7823";
const RECONNECT_MS = 5000;
const POLL_ALARM  = "ramwarden-poll";

const IS_FIREFOX = navigator.userAgent.includes("Firefox");

// ── Firefox alarm listener — registered at TOP LEVEL so it survives event-page restarts.
// When Firefox suspends the event page and wakes it for an alarm, this listener is
// re-registered before the alarm event fires. setInterval cannot survive suspension.
if (IS_FIREFOX && chrome.alarms) {
  chrome.alarms.onAlarm.addListener((alarm) => {
    if (alarm.name === POLL_ALARM) _poll();
  });
}

let _host    = DEFAULT_HOST;
let _wsUrl   = `ws://${DEFAULT_HOST}/ws`;
let _httpUrl = `http://${DEFAULT_HOST}`;

// Stable per-browser ID persisted in local storage.
// NOT held in memory as a module-level var — event page restarts clear memory.
// _poll() always reads it fresh from storage to avoid the null race.
let ws = null;

// ── URL protection ────────────────────────────────────────────────────────────

const PROTECTED_PREFIXES = [
  "chrome://", "brave://", "about:", "chrome-extension://",
  "moz-extension://", "devtools://", "edge://", "opera://",
  "vivaldi://", "data:", "javascript:", "file://",
];

const PROTECTED_PATTERNS = [
  /^https?:\/\/localhost/i,
  /^https?:\/\/127\./,
  /^https?:\/\/192\.168\./,
  /^https?:\/\/10\.\d/,
  /\/login|\/signin|\/auth|\/oauth|\/callback|\/session/i,
  /accounts?\.(google|microsoft|apple|github)\./i,
  /console\.|dashboard\.|admin\./i,
];

function isProtected(url) {
  if (!url) return true;
  if (PROTECTED_PREFIXES.some(p => url.startsWith(p))) return true;
  if (PROTECTED_PATTERNS.some(r => r.test(url))) return true;
  return false;
}

// ── Tab activity tracking ─────────────────────────────────────────────────────

let tabActivity = {};

chrome.tabs.onActivated.addListener(({ tabId }) => {
  chrome.tabs.get(tabId, (tab) => {
    if (chrome.runtime.lastError || !tab || isProtected(tab.url)) return;
    tabActivity[tabId] = {
      url: tab.url || "",
      title: tab.title || tab.url || "",
      lastActiveMs: Date.now(),
      incognito: tab.incognito || false,
    };
    persistActivity();
  });
});

chrome.tabs.onUpdated.addListener((tabId, changeInfo, tab) => {
  if (changeInfo.status !== "complete") return;
  if (isProtected(tab.url)) {
    if (tabActivity[tabId]) { delete tabActivity[tabId]; persistActivity(); }
    return;
  }
  tabActivity[tabId] = {
    url: tab.url || "",
    title: tab.title || tab.url || "",
    lastActiveMs: tabActivity[tabId]?.lastActiveMs ?? (tab.lastAccessed || Date.now()),
    incognito: tab.incognito || false,
  };
  persistActivity();
});

chrome.tabs.onRemoved.addListener((tabId) => {
  delete tabActivity[tabId];
  persistActivity();
});

function persistActivity() {
  chrome.storage.local.set({ tabActivity });
}

chrome.storage.local.get("tabActivity", (r) => {
  if (r.tabActivity) tabActivity = r.tabActivity;
});

// Seed on load using real lastAccessed timestamps from the browser.
// Waits for the storage write to confirm before polling so _poll() sees the data.
chrome.tabs.query({}, (allTabs) => {
  const now = Date.now();
  const FALLBACK_INACTIVE_MS = 30 * 60 * 1000;
  let seeded = 0;
  for (const tab of allTabs) {
    if (isProtected(tab.url)) continue;
    if (!tabActivity[tab.id]) {
      tabActivity[tab.id] = {
        url: tab.url || "",
        title: tab.title || tab.url || "",
        lastActiveMs: tab.lastAccessed || (now - FALLBACK_INACTIVE_MS),
        incognito: tab.incognito || false,
      };
      seeded++;
    }
  }
  console.log(`[RamWarden] seeded ${seeded} tabs (${allTabs.length} total, ${Object.keys(tabActivity).length} tracked)`);
  // Wait for storage write to complete, then poll — avoids race where _poll() reads
  // before persistActivity()'s async write has committed.
  chrome.storage.local.set({ tabActivity }, () => {
    if (IS_FIREFOX) _poll();
  });
});

// ── Shared helpers ────────────────────────────────────────────────────────────

function buildTabReport(activity) {
  const now = Date.now();
  return Object.entries(activity)
    .map(([id, d]) => ({
      id: parseInt(id),
      url: d.url,
      title: d.title,
      lastActiveMs: d.lastActiveMs,
      inactiveMinutes: Math.round((now - d.lastActiveMs) / 60000),
      incognito: d.incognito || false,
    }))
    .filter(t => !isProtected(t.url));
}

function closeTabs(tabIds) {
  if (!tabIds?.length) return;
  const safeIds = tabIds.filter(id => {
    const entry = tabActivity[id];
    return entry && !isProtected(entry.url);
  });
  if (!safeIds.length) return;
  chrome.tabs.remove(safeIds, () => {
    if (IS_FIREFOX) {
      _poll();
    } else {
      _send({ action: "tabs_closed", tabIds: safeIds });
    }
  });
}

function _badge(connected) {
  chrome.action.setBadgeText({ text: connected ? "" : "!" });
  chrome.action.setBadgeBackgroundColor({ color: connected ? "#22c55e" : "#ef4444" });
}

// ── Load config, then start the right transport ───────────────────────────────

chrome.storage.sync.get("daemonHost", (r) => {
  if (r.daemonHost) {
    _host    = r.daemonHost;
    _wsUrl   = `ws://${_host}/ws`;
    _httpUrl = `http://${_host}`;
  }
  if (IS_FIREFOX) {
    _startPolling();
  } else {
    _connect();
  }
});

// ── Firefox: HTTP polling ─────────────────────────────────────────────────────

async function _poll() {
  // Load browserId + tabActivity together from storage.
  // Always reads from storage (not in-memory) so alarm-triggered wakeups work
  // even though in-memory state is cleared on event page restart.
  const localData = await new Promise(r => chrome.storage.local.get(["browserId", "tabActivity"], r));
  let browserId = localData.browserId;
  if (!browserId) {
    browserId = "rw-" + Math.random().toString(36).slice(2, 10);
    chrome.storage.local.set({ browserId });
  }

  // Re-read host config if this is a fresh event page wake
  if (_httpUrl === `http://${DEFAULT_HOST}`) {
    const syncData = await new Promise(r => chrome.storage.sync.get("daemonHost", r));
    if (syncData.daemonHost) _httpUrl = `http://${syncData.daemonHost}`;
  }

  const activity = localData.tabActivity || {};
  const tabs = buildTabReport(activity);
  console.log(`[RamWarden] poll → ${tabs.length} tabs to report (activity keys: ${Object.keys(activity).length})`);

  try {
    const resp = await fetch(_httpUrl + "/api/tabs", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ browser_id: browserId, tabs }),
    });
    if (!resp.ok) {
      console.warn("[RamWarden] poll failed: HTTP", resp.status);
      _badge(false);
      return;
    }
    _badge(true);
    const data = await resp.json();
    for (const cmd of (data.commands || [])) {
      if (cmd.action === "close") closeTabs(cmd.tabIds);
    }
  } catch (e) {
    console.warn("[RamWarden] poll fetch error:", e);
    _badge(false);
  }
}

function _startPolling() {
  if (chrome.alarms) {
    // periodInMinutes: Firefox minimum is 1 min; Chrome allows 0.5 in dev mode.
    // We use 0.5 — Firefox clamps to 1 min silently, Chrome uses it as-is.
    chrome.alarms.create(POLL_ALARM, { periodInMinutes: 0.5 });
  }
  // Initial poll is triggered by the tabs.query seeding callback above,
  // not here — that ensures tabActivity is populated before the first send.
}

// ── Chrome/Brave: WebSocket ───────────────────────────────────────────────────

function _connect() {
  if (ws && (ws.readyState === WebSocket.CONNECTING || ws.readyState === WebSocket.OPEN)) return;
  try {
    ws = new WebSocket(_wsUrl);
  } catch {
    setTimeout(_connect, RECONNECT_MS);
    return;
  }

  ws.onopen = () => {
    console.log("[RamWarden] WS connected to", _wsUrl);
    _badge(true);
  };

  ws.onmessage = (ev) => {
    let msg;
    try { msg = JSON.parse(ev.data); } catch { return; }
    switch (msg.action) {
      case "get_tabs": _sendTabReport(); break;
      case "close":    closeTabs(msg.tabIds); break;
      case "ping":     _send({ action: "pong" }); break;
    }
  };

  ws.onclose = () => { _badge(false); setTimeout(_connect, RECONNECT_MS); };
  ws.onerror = () => { ws.close(); };
}

function _sendTabReport() {
  chrome.storage.local.get("tabActivity", (r) => {
    _send({ action: "tab_report", tabs: buildTabReport(r.tabActivity || {}) });
  });
}

function _send(obj) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(obj));
}

// Keep WS alive (Chrome/Brave only)
if (!IS_FIREFOX) {
  setInterval(() => {
    if (ws?.readyState === WebSocket.OPEN) _send({ action: "ping" });
    else _connect();
  }, 25000);
}
