const DAEMON_HTTP = "http://localhost:7823";

// Load persisted daemon host setting
const daemonReady = new Promise(resolve => chrome.storage.sync.get("daemonHost", (r) => {
  if (r.daemonHost) {
    window.DAEMON_HTTP = "http://" + r.daemonHost;
  }
  resolve();
}));

async function daemonJSON(path, opts = {}) {
  await daemonReady;
  const base = window.DAEMON_HTTP || DAEMON_HTTP;
  const r = await fetch(base + path, {signal:AbortSignal.timeout(8000),...opts});
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

  document.getElementById("analyze-btn").addEventListener("click", analyzeNow);

  // Daemon host setting
  chrome.storage.sync.get("daemonHost", (r) => {
    document.getElementById("daemon-host-input").value = r.daemonHost || "localhost:7823";
  });
}

// ── The warden ────────────────────────────────────────────────────────────────

async function wardenPost(path, body, timeout = 40000) {
  const r = await fetch((window.DAEMON_HTTP || DAEMON_HTTP) + path, {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body), signal: AbortSignal.timeout(timeout),
  });
  return { status: r.status, body: await r.json().catch(() => ({})) };
}

async function refreshWarden() {
  const el = document.getElementById("warden");
  let view = null;
  try { view = wardenView(await daemonJSON("/warden/bubble")); } catch { /* older daemon: no voice */ }
  el.innerHTML = wardenHTML(view);
  el.hidden = !view;
  el.dataset.id = view?.id || "";
}

function wardenSay(text) {
  const el = document.getElementById("warden");
  if (!text) return refreshWarden();
  el.innerHTML = wardenHTML({ text, buttons: [], muted: true });
  el.hidden = false;
  setTimeout(refreshWarden, 6000);
}

document.getElementById("warden").addEventListener("click", async (e) => {
  const b = e.target.closest("button[data-choice]");
  if (!b) return;
  const el = document.getElementById("warden");
  el.querySelectorAll("button").forEach(x => { x.disabled = true; });
  try {
    if (b.dataset.choice === "unmute") {
      await wardenPost("/warden/mute", { muted: false });
      return refreshWarden();
    }
    const r = await wardenPost("/warden/answer", { id: el.dataset.id, choice: b.dataset.choice });
    chrome.runtime.sendMessage?.({ type: "warden-answered" });
    wardenSay(r.body.reply);
  } catch { wardenSay("I couldn't reach the daemon."); }
});

document.getElementById("warden").addEventListener("change", async (e) => {
  if (!e.target.matches("select.warden-mute") || e.target.value === "") return;
  const [, hours] = WARDEN_MUTE[Number(e.target.value)];
  try {
    const r = await wardenPost("/warden/mute", { muted: true, hours });
    wardenSay(r.body.reply);
  } catch { wardenSay("I couldn't reach the daemon."); }
});

// "Analyze now": the warden reads the tabs and memory and says what it would
// release, with buttons for its suggestions. Nothing happens until one is pressed.
let lastAnalysis = null;
async function analyzeNow() {
  const btn = document.getElementById("analyze-btn");
  const out = document.getElementById("analysis-result");
  btn.disabled = true;
  btn.textContent = "Your warden is looking…";
  try {
    let r = await wardenPost("/browser/explain", {}, 60000);
    if (r.status === 404) { // an older daemon: the rules' count only
      r = await wardenPost("/browser/analyze", {}, 30000);
      r.body = { reply: `${r.body.tabs.length} tabs inspected; ${r.body.candidates} can be unloaded.`, tier: "facts" };
    } else if (r.status !== 200) throw new Error(r.status);
    lastAnalysis = r.body;
    out.innerHTML = analysisHTML(r.body);
    out.hidden = false;
  } catch {
    out.innerHTML = analysisHTML({ reply: "I couldn't reach the daemon, so I have no opinion yet.", tier: "" });
    out.hidden = false;
  }
  btn.disabled = false;
  btn.textContent = "Analyze Now";
}

document.getElementById("analysis-result").addEventListener("click", async (e) => {
  const b = e.target.closest("button[data-act]");
  if (!b || !lastAnalysis) return;
  const act = b.dataset.act;
  const targets = (lastAnalysis[act] || []).map(({ browser, id, url }) => ({ browser, id, url }));
  if (act === "close" && !confirm(`Close ${targets.length} tab(s)? They go in the reopen list below.`)) return;
  b.disabled = true;
  try {
    const r = await wardenPost(act === "close" ? "/browser/close" : "/browser/discard", { targets });
    const done = (r.body.confirmed || []).length, queued = (r.body.queued || []).length;
    b.textContent = act === "close" ? `Closed ${done}${queued ? `, ${queued} pending` : ""}` : `Unloaded ${done}${queued ? `, ${queued} pending` : ""}`;
  } catch { b.textContent = "Daemon offline"; }
});

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
refreshWarden();


async function refreshThreat() {
 const headline=document.getElementById('threat-headline');
 const detail=document.getElementById('threat-detail');
 const engines=document.getElementById('threat-engines');
 const alerts=document.getElementById('threat-alerts');
 headline.textContent='Checking protection…';engines.replaceChildren();alerts.replaceChildren();
 try {
  const data=await daemonJSON('/wardens/threat');
  headline.textContent=data.status?.headline || 'Protection unverified';detail.textContent=data.status?.detail || '';
  for(const engine of data.report?.engines || []){const row=document.createElement('p');row.textContent=`${engine.name}: ${engine.state}`;engines.append(row);}
  for(const alert of (data.report?.alerts || []).slice(0,50)){
   const card=document.createElement('article');card.className='security-alert';
   const title=document.createElement('strong');title.textContent=`Level ${alert.level} · Rule ${alert.rule_id}`;
   const body=document.createElement('p');body.textContent=String(alert.description || 'No description supplied').slice(0,1200);
   const time=document.createElement('time');time.textContent=alert.timestamp || 'Time unavailable';
   card.append(title,body,time);for(const [name,value] of Object.entries(alert.evidence||{}).slice(0,8)){const row=document.createElement('p');row.textContent=`${name}: ${String(value).slice(0,512)}`;card.append(row);}if(alert.advice){const advice=document.createElement('p');advice.textContent=alert.advice;card.append(advice);}alerts.append(card);
  }
  if(data.report?.alerts?.length){const note=document.createElement('p');note.textContent='Reported rule alerts are not confirmed malware. Showing at most 50 recent alerts.';alerts.prepend(note);}
  else if(data.unlocked && data.report){alerts.textContent='No alerts returned in this report. This does not prove the computer is free of threats.';}
 } catch {headline.textContent='Protection unverified';detail.textContent='Install or reconnect the desktop daemon to read this computer’s security report. An extension or Pro pass alone does not prove protection.';}
}
for(const kind of ['ram','threat'])document.getElementById(kind+'-tab').addEventListener('click',()=>{
 for(const id of ['ram','threat']){document.getElementById(id+'-pane').hidden=id!==kind;document.getElementById(id+'-tab').setAttribute('aria-selected',String(id===kind));}
 if(kind==='threat')refreshThreat();
});
document.getElementById('threat-refresh').addEventListener('click',refreshThreat);
document.getElementById('save-daemon-host').addEventListener('click',saveDaemonHost);

chrome.storage.local.get(['liteEnabled','liteBudget'],settings=>{document.getElementById('lite-enabled').checked=settings.liteEnabled===true;document.getElementById('lite-budget').value=settings.liteBudget||6;});
document.getElementById('lite-enabled').addEventListener('change',event=>chrome.storage.local.set({liteEnabled:event.target.checked}));
document.getElementById('lite-budget').addEventListener('change',event=>{const n=Math.max(1,Math.min(50,Math.floor(Number(event.target.value)||6)));event.target.value=n;chrome.storage.local.set({liteBudget:n});});

(async()=>{try{const data=await daemonJSON('/wardens');const link=document.getElementById('account-link');if(data.paid && data.tier==='pro')link.textContent='Pro 🐐 · Account and billing';else if(data.paid && data.tier==='ram')link.textContent='Plus ⚡ · Account and billing';}catch{/* Standalone Lite remains usable without a desktop account. */}})();
