const DAEMON_HTTP = "http://localhost:7823";

// Load persisted daemon host setting
chrome.storage.sync.get("daemonHost", (r) => {
  if (r.daemonHost) {
    window.DAEMON_HTTP = "http://" + r.daemonHost;
  }
});

async function daemonJSON(path, opts = {}) {
  const base = window.DAEMON_HTTP || DAEMON_HTTP;
  const r = await fetch(base + path, opts);
  if (!r.ok) throw new Error(r.status);
  return r.json();
}

// ── RAM stats ─────────────────────────────────────────────────────────────────

async function init() {
  try {
    const stats = await daemonJSON("/stats");
    setConnected(true);
    const pct = stats.percent;
    document.getElementById("ram-pct").textContent = pct.toFixed(0) + "%";
    document.getElementById("ram-used").textContent =
      `${stats.used_mb.toFixed(0)} MB / ${stats.total_mb.toFixed(0)} MB`;
    const fill = document.getElementById("ram-fill");
    fill.style.width = pct + "%";
    fill.className = "ram-bar-fill" + (pct >= 85 ? " danger" : pct >= 75 ? " warn" : "");
    document.getElementById("browsers-connected").textContent = stats.browsers_connected ?? "?";
  } catch {
    setConnected(false);
    document.getElementById("ram-pct").textContent = "offline";
  }

  // Load RAM breakdown from profiler
  try {
    const report = await daemonJSON("/ram-report");
    renderRamBreakdown(report);
  } catch { /* daemon may not have profiler data yet */ }

  chrome.storage.local.get("tabActivity", (result) => {
    const count = Object.keys(result.tabActivity || {}).length;
    document.getElementById("tab-count").textContent = `${count} tabs tracked`;
  });

  // Check incognito access — show a hint if not enabled
  if (chrome.extension?.isAllowedIncognitoAccess) {
    chrome.extension.isAllowedIncognitoAccess((allowed) => {
      if (!allowed) {
        const el = document.getElementById("tab-count");
        el.innerHTML += ` &nbsp;<span style="color:#f59e0b;font-size:11px" title="Private/incognito tabs are not tracked">
          ⚠ Enable private access in browser extension settings to track incognito tabs</span>`;
      }
    });
  }

  // Tabs RamWarden closed, newest first: the extension's own list merged with
  // the daemon's closed_tabs record, which also covers closes made before the
  // extension was reloaded.
  const local = await new Promise(r => chrome.storage.local.get("releasedTabs", r));
  let daemon = [];
  try { daemon = (await daemonJSON("/history")).closed_tabs || []; } catch { /* offline */ }
  const seen = new Set();
  const merged = [
    ...(local.releasedTabs || []).map(t => ({ url: t.url, title: t.title, closedMs: t.closedAt })),
    ...daemon.map(t => ({ url: t.url, title: t.title, closedMs: Date.parse(t.closed_at.endsWith("Z") ? t.closed_at : t.closed_at + "Z") })),
  ].filter(t => t.url && !seen.has(t.url) && seen.add(t.url))
   .sort((a, b) => (b.closedMs || 0) - (a.closedMs || 0));
  renderHistory(merged);

  document.getElementById("analyze-btn").addEventListener("click", async () => {
    const btn = document.getElementById("analyze-btn");
    btn.disabled = true;
    btn.textContent = "Analyzing…";
    try {
      const response = await fetch((window.DAEMON_HTTP || DAEMON_HTTP) + "/browser/analyze", {
        method: "POST", signal: AbortSignal.timeout(30000),
      });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const analysis = await response.json();
      document.getElementById("analysis-result").textContent =
        `${analysis.tabs.length} tabs inspected; ${analysis.candidates} can be unloaded. Review and select them in RamWarden → Browser tabs.`;
      btn.textContent = "Analysis ready";
    } catch {
      btn.textContent = "Daemon offline";
    }
    setTimeout(() => { btn.disabled = false; btn.textContent = "Analyze Now"; }, 3000);
  });

  // Daemon host setting
  chrome.storage.sync.get("daemonHost", (r) => {
    document.getElementById("daemon-host-input").value = r.daemonHost || "localhost:7823";
  });
}

function renderRamBreakdown(report) {
  const el = document.getElementById("ram-breakdown");
  if (!report?.by_category) { el.textContent = "No profiler data yet"; return; }

  const CAT_LABEL = {
    BROWSER: "🌐 Browsers",
    WORK_TOOL: "🔧 Work tools",
    USER_IDLE: "💤 Idle apps",
    AGENT: "🤖 Agents",
    SYSTEM: "⚙️ System",
    COMPOSITOR: "🖥️ Desktop",
  };
  const rows = Object.entries(report.by_category)
    .map(([cat, info]) => ({ cat, mb: info.total_mb, count: info.count }))
    .filter(r => r.mb > 50)
    .sort((a, b) => b.mb - a.mb);

  const lines = rows.map(r => {
    const gb = (r.mb / 1024).toFixed(1);
    const label = CAT_LABEL[r.cat] || r.cat;
    const barPct = Math.min(100, r.mb / (report.ram?.total_gb * 10));
    return `<div style="display:flex;justify-content:space-between;margin-bottom:1px">
      <span>${label} <span style="color:#475569">(${r.count})</span></span>
      <span style="color:#e2e8f0;font-weight:600">${gb} GB</span>
    </div>`;
  });

  const avail = report.ram?.available_gb?.toFixed(1) ?? "?";
  lines.push(`<div style="margin-top:4px;padding-top:4px;border-top:1px solid #1e293b;color:#22c55e">
    ✓ ${avail} GB truly available</div>`);

  if (report.suspend_candidates?.length) {
    const names = report.suspend_candidates.slice(0,3).map(c => c.name).join(", ");
    lines.push(`<div style="margin-top:2px;color:#f59e0b">
      ⚠ Idle: ${names}</div>`);
  }

  el.innerHTML = lines.join("");
}

// Inline onclick handlers are blocked by the MV3 content security policy, so
// reopening goes through one delegated listener.
document.getElementById("history-list").addEventListener("click", (e) => {
  const a = e.target.closest("a[data-url]");
  if (!a) return;
  e.preventDefault();
  chrome.tabs.create({ url: a.dataset.url });
});

function setConnected(ok) {
  document.getElementById("status-dot").className = ok ? "connected" : "";
}

function renderHistory(items) {
  const list = document.getElementById("history-list");
  if (!items.length) {
    list.innerHTML = '<span style="color:#475569">No tabs closed yet. Unloaded tabs stay in the tab strip and show in the badge count.</span>';
    return;
  }
  list.innerHTML = items.slice(0, 20).map((item) => {
    const time = new Date(item.closedMs || Date.now()).toLocaleString(undefined, {
      month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
    });
    return `<div class="history-item">
      <a href="${esc(item.url)}" data-url="${esc(item.url)}" title="Reopen ${esc(item.title)}">
        ${esc(item.title || item.url)}
      </a>
      <span class="history-time">${time}</span>
    </div>`;
  }).join("");
}

// ── Daemon host ───────────────────────────────────────────────────────────────

const LOCAL_HOSTS = /^(localhost|127\.0\.0\.1)(:\d+)?$/i;

function saveDaemonHost() {
  const val = document.getElementById("daemon-host-input").value.trim() || "localhost:7823";
  const commit = () =>
    chrome.storage.sync.set({ daemonHost: val }, () => chrome.runtime.reload());

  // localhost is covered by the manifest. Anything else — a Tailscale address,
  // another machine on the LAN — falls under the optional host permission, which
  // the store build does not grant up front. Ask for it before saving, or the
  // extension would silently fail to reach the daemon it was just pointed at.
  if (LOCAL_HOSTS.test(val) || !chrome.permissions) return commit();

  chrome.permissions.request({ origins: [`http://${val}/*`] }, (granted) => {
    if (!granted) {
      const el = document.getElementById("daemon-host-status");
      if (el) el.textContent = "Permission denied — host not saved.";
      return;
    }
    commit();
  });
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function esc(s) {
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

init();
