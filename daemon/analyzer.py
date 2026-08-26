"""
RAM analysis — tiered approach:
  Tier 1  RAM < warn_percent:  do nothing
  Tier 2  warn ≤ RAM < critical: heuristic-only (no API cost)
  Tier 3  RAM ≥ critical OR manual trigger: full Claude analysis

This means the API is only called when RAM is actually in trouble.
"""
import json
import logging
import re
from dataclasses import dataclass, field

import anthropic

from . import config
from .monitor import RAMSnapshot

log = logging.getLogger("ramwarden.analyzer")

# URLs/domains we never auto-close regardless of inactivity
NEVER_CLOSE_PATTERNS = [
    r"localhost", r"127\.0\.0", r"192\.168\.", r"10\.0\.",  # local/dev
    r"github\.com/.*/issues", r"github\.com/.*/pull",       # open PRs/issues
    r"console\.", r"dashboard\.", r"admin\.",                 # consoles
    r"accounts\.google", r"login\.", r"signin\.",             # auth pages
]

# Internal browser pages — never reach us via the extension, but guard anyway
INTERNAL_URL_PREFIXES = (
    "chrome://", "brave://", "about:", "chrome-extension://",
    "moz-extension://", "devtools://", "edge://", "data:", "javascript:",
)

# Domains that are very likely "I'll read this later but never will"
STALE_DOMAINS = [
    "youtube.com", "reddit.com", "twitter.com", "x.com",
    "instagram.com", "tiktok.com", "news.ycombinator.com",
    "medium.com", "substack.com", "hackernews",
]

SYSTEM_PROMPT = """You are a RAM management assistant for a Linux desktop.

You receive browser tab data, process info, terminal shell info, and optionally the user's current goal.
Your job: recommend which STALE tabs to close and which idle processes to suspend.

RULES — read carefully:
1. Tab closing:
   - ONLY close tabs inactive > the stated threshold.
   - DO NOT close tabs that look like active work sessions: auth pages, localhost/dev servers, anything that looks like the user is mid-task.
   - DO close: background social media, YouTube videos, Reddit threads, news articles, job listings, shopping tabs — things the user probably forgot about.
   - If multiple tabs are open on the same domain, only close the oldest/most inactive ones.
   - Be assertive but not reckless.
   - NEVER close: devtools://, chrome://, about:, moz-extension://, or any internal browser page.

2. When a USER GOAL is provided:
   - Tabs RELEVANT to the goal → KEEP, even if recently inactive.
   - Tabs CLEARLY IRRELEVANT to the goal → CLOSE, even if recently visited.
   - Example: goal="bug bounty" → keep CVE databases, security tools, HackerOne tabs; close job listings, recipe sites, YouTube.
   - When in doubt, keep the tab and note it in the summary.

3. Process suspension:
   - ONLY name processes from the RECLAIMABLE list below. It is already filtered by
     the watchlist and by live activity signals — anything absent from it will be
     refused by the daemon regardless of what you say, so naming it wastes the turn.
   - The PROTECTED list explains what is off-limits and why. Do not argue with it.
   - Prefer suspending nothing over suspending something the user is mid-way through.

4. Idle terminals:
   - idle_terminals_to_close: ONLY PIDs with zero child processes.
   - NEVER include busy shells (running agents, servers, etc.).

5. Workspace sorting:
   - workspaces_to_sort: only true if the layout is messy and rules are configured.

6. Format: respond with ONLY this JSON, no prose, no fences:
{"tabs_to_close":[],"processes_to_suspend":[],"idle_terminals_to_close":[],"workspaces_to_sort":false,"estimated_ram_freed_mb":0,"summary":"<one sentence>"}"""


# Constrains the reply to exactly the shape below. Without this the model is free
# to answer in prose — which it does when the tab list prompts a comment — and the
# analysis falls back to heuristics for a reason that has nothing to do with RAM.
RECOMMENDATION_SCHEMA = {
    "type": "object",
    "properties": {
        "tabs_to_close": {"type": "array", "items": {"type": "integer"}},
        "processes_to_suspend": {"type": "array", "items": {"type": "string"}},
        "idle_terminals_to_close": {"type": "array", "items": {"type": "integer"}},
        "workspaces_to_sort": {"type": "boolean"},
        "estimated_ram_freed_mb": {"type": "number"},
        "summary": {"type": "string"},
    },
    "required": [
        "tabs_to_close", "processes_to_suspend", "idle_terminals_to_close",
        "workspaces_to_sort", "estimated_ram_freed_mb", "summary",
    ],
    "additionalProperties": False,
}


@dataclass
class Recommendation:
    tabs_to_close: list[int] = field(default_factory=list)
    processes_to_suspend: list[str] = field(default_factory=list)
    idle_terminals_to_close: list[int] = field(default_factory=list)
    workspaces_to_sort: bool = False
    estimated_ram_freed_mb: float = 0.0
    summary: str = ""
    tier: str = "none"  # "none" | "heuristic" | "claude"


def _activity_section() -> tuple[str, set[str]]:
    """
    Render the live activity picture for the prompt, and return the set of process
    names that are actually reclaimable so the reply can be clamped to it.
    """
    from .activity import get_detector

    det = get_detector()
    if not det.snapshot():
        return "(activity detector still warming up — suspend nothing)", set()

    cfg = config.get()
    reclaimable = det.reclaimable(cfg.watchlist)
    allowed = {s.name for s in reclaimable}

    protected = [
        s for s in sorted(det.snapshot().values(), key=lambda s: s.rss_mb, reverse=True)
        if s.verdict == "PROTECTED" and s.rss_mb >= 100
    ][:12]

    lines = ["PROTECTED — never suggest these:"]
    lines += [f"  - {s.name} ({s.rss_mb:.0f} MB): {s.reasons[0]}" for s in protected] or ["  none"]
    lines.append("")
    lines.append("RECLAIMABLE — the only names you may put in processes_to_suspend:")
    lines += [
        f"  - {s.name} ({s.rss_mb:.0f} MB): {'; '.join(s.reasons)}" for s in reclaimable
    ] or ["  none — suspend nothing this round"]
    return "\n".join(lines), allowed


def analyze(
    snap: RAMSnapshot,
    tabs: list[dict],
    terminals=None,
    workspace_layout=None,
    force_claude: bool = False,
    goal_context: str = "",
) -> Recommendation:
    """
    Tiered analysis.
    force_claude=True skips the RAM threshold check (used for the manual 'Analyze Now' button).
    goal_context is set when the user re-analyzes from within the dialog.
    """
    cfg = config.get()
    warn_pct = cfg.thresholds.ram_percent
    critical_pct = cfg.thresholds.critical_percent

    # Tier 1: RAM is fine — do nothing automatically
    if not force_claude and not goal_context and snap.percent < warn_pct:
        log.info("RAM at %.0f%% (< %.0f%% threshold) — skipping analysis", snap.percent, warn_pct)
        return Recommendation(summary=f"RAM at {snap.percent:.0f}% — all good", tier="none")

    # Tier 2: Elevated RAM — use heuristics, no API call
    # Skip tier 2 if a goal was provided (goal always warrants Claude)
    if not force_claude and not goal_context and snap.percent < critical_pct:
        log.info("RAM at %.0f%% — using heuristic (no Claude API call)", snap.percent)
        return _heuristic_fallback(snap, tabs, cfg.thresholds.inactivity_minutes, tier="heuristic")

    # Tier 3: Critical RAM, manual trigger, or goal-aware re-analysis
    if not cfg.anthropic_api_key:
        log.warning("No API key — falling back to heuristics")
        return _heuristic_fallback(snap, tabs, cfg.thresholds.inactivity_minutes, tier="heuristic")

    try:
        return _claude_analyze(snap, tabs, cfg, terminals, workspace_layout, goal_context=goal_context)
    except Exception as e:
        log.warning("Claude API error (%s) — falling back to heuristics", e)
        return _heuristic_fallback(snap, tabs, cfg.thresholds.inactivity_minutes, tier="heuristic")


def _is_protected(url: str) -> bool:
    """Return True if this URL should never be auto-closed."""
    if not url:
        return True
    if any(url.startswith(prefix) for prefix in INTERNAL_URL_PREFIXES):
        return True
    url_lower = url.lower()
    return any(re.search(p, url_lower) for p in NEVER_CLOSE_PATTERNS)


def _is_stale_domain(url: str) -> bool:
    """Return True if this domain is a good candidate for closing when inactive."""
    url_lower = url.lower()
    return any(d in url_lower for d in STALE_DOMAINS)


def _heuristic_fallback(snap: RAMSnapshot, tabs: list[dict], threshold_min: int,
                        tier: str = "heuristic") -> Recommendation:
    """
    Simple rule-based tab selection — no API calls.
    Priority: stale-domain tabs first, then any tab past 2x the threshold.
    Never closes protected URLs.
    """
    stale = []
    for t in tabs:
        url = t.get("url", "")
        inactive = t.get("inactiveMinutes", 0)
        if inactive < threshold_min:
            continue
        if _is_protected(url):
            continue
        # Be more aggressive on stale domains, conservative on others
        if _is_stale_domain(url) and inactive >= threshold_min:
            stale.append(t)
        elif inactive >= threshold_min * 2:  # only close non-stale sites at 2x threshold
            stale.append(t)

    stale.sort(key=lambda t: t.get("inactiveMinutes", 0), reverse=True)

    summary = (
        f"{len(stale)} stale tab(s) identified by rules (RAM {snap.percent:.0f}%)."
        if stale else f"RAM at {snap.percent:.0f}%, no stale tabs qualify under heuristic rules."
    )
    return Recommendation(
        tabs_to_close=[t["id"] for t in stale],
        estimated_ram_freed_mb=0.0,
        summary=summary,
        tier=tier,
    )


def _extract_json(raw: str) -> dict:
    """
    Pull the JSON object out of a model reply.

    The prompt asks for bare JSON, but a reply occasionally arrives wrapped in a
    markdown fence or with a sentence in front of it. Rather than special-casing
    each wrapper, take the outermost brace pair — that is the object regardless of
    what surrounds it.
    """
    text = raw.strip()
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        pass

    start = text.find("{")
    end = text.rfind("}")
    if start != -1 and end > start:
        try:
            return json.loads(text[start:end + 1])
        except json.JSONDecodeError:
            pass

    log.warning("Could not parse a JSON object from the reply: %.400r", text)
    raise ValueError("no JSON object in model reply")


def _claude_analyze(snap: RAMSnapshot, tabs: list[dict], cfg, terminals=None, workspace_layout=None, goal_context: str = "") -> Recommendation:
    proc_summary = "\n".join(
        f"  - {p.name}: {p.rss_mb:.0f} MB [status: {p.status}]" for p in snap.processes[:10]
    )
    tab_summary = "\n".join(
        f"  - id={t['id']} inactive={t.get('inactiveMinutes', 0)}min url={t.get('url', '')} title=\"{t.get('title', '')}\""
        for t in tabs
    ) or "  (no tabs reported)"

    term_summary = "(not collected)"
    if terminals is not None:
        idle = [t for t in terminals if t.is_idle]
        busy = [t for t in terminals if not t.is_idle]
        lines = (
            [f"  - PID={t.pid} ({t.shell_name}) — IDLE, safe to close" for t in idle] +
            [f"  - PID={t.pid} ({t.shell_name}) children: {', '.join(t.child_processes[:3])} — DO NOT CLOSE" for t in busy]
        )
        term_summary = "\n".join(lines) or "  none found"

    activity_summary, allowed_names = _activity_section()

    ws_summary = "(not collected)"
    if workspace_layout is not None:
        ws_summary = f"{workspace_layout.n_workspaces} workspaces, {len(workspace_layout.windows)} windows"

    goal_section = (
        f"\nUSER'S CURRENT GOAL: \"{goal_context}\"\n"
        "Use this goal to decide relevance — keep goal-related tabs, close unrelated ones.\n"
        if goal_context else ""
    )

    user_msg = f"""RAM: {snap.percent:.1f}% ({snap.used_mb:.0f} MB / {snap.total_mb:.0f} MB)
Inactivity threshold: {cfg.thresholds.inactivity_minutes} min
{goal_section}
Top processes:
{proc_summary}

What the user is actually using right now:
{activity_summary}

Browser tabs:
{tab_summary}

Terminal shells:
{term_summary}

Workspace: {ws_summary}

Return your JSON recommendation."""

    client = anthropic.Anthropic(api_key=cfg.anthropic_api_key)
    message = client.messages.create(
        model="claude-sonnet-4-6",
        max_tokens=1024,
        system=SYSTEM_PROMPT,
        messages=[{"role": "user", "content": user_msg}],
        output_config={"format": {"type": "json_schema", "schema": RECOMMENDATION_SCHEMA}},
    )

    raw = next(
        (b.text for b in message.content if getattr(b, "type", None) == "text"), ""
    ).strip()
    if not raw:
        raise ValueError(f"no text in reply (stop_reason={message.stop_reason})")

    data = _extract_json(raw)

    # Clamp suspensions to what the activity gate would actually allow. The daemon
    # enforces this again before signalling, but dropping it here keeps the UI from
    # promising the user something that will then be refused.
    suggested = data.get("processes_to_suspend", []) or []
    approved = [n for n in suggested if n in allowed_names]
    for rejected in set(suggested) - set(approved):
        log.warning("Ignoring suggested suspend %r — not in the reclaimable set", rejected)

    return Recommendation(
        tabs_to_close=data.get("tabs_to_close", []),
        processes_to_suspend=approved,
        idle_terminals_to_close=data.get("idle_terminals_to_close", []),
        workspaces_to_sort=bool(data.get("workspaces_to_sort", False)),
        estimated_ram_freed_mb=data.get("estimated_ram_freed_mb", 0.0),
        summary=data.get("summary", ""),
        tier="claude",
    )
