"""
Process profiler — maintains a rolling CPU sample window and classifies
each process so RamWarden can make smart decisions without calling Claude.

Classification:
  COMPOSITOR   — desktop compositor, display server (never touch)
  SYSTEM       — low-level system daemon, kernel thread
  AGENT        — running AI agent / important user work (never touch)
  WORK_TOOL    — known dev/security tool (never touch)
  BROWSER      — browser renderer (handled via tab extension)
  USER_ACTIVE  — recently had CPU activity (leave alone)
  USER_IDLE    — user app, high RAM, no recent CPU → suspend candidate
  BACKGROUND   — small service, mostly harmless

Samples are kept for 10 minutes (every 10s poll = 60 samples).
"""
import logging
import os
import time
from collections import deque
from dataclasses import dataclass, field
from typing import Literal

import psutil

log = logging.getLogger("ramwarden.profiler")

Category = Literal[
    "COMPOSITOR", "SYSTEM", "AGENT", "WORK_TOOL",
    "BROWSER", "USER_ACTIVE", "USER_IDLE", "BACKGROUND",
]

# ── Name-based classification rules ──────────────────────────────────────────

_COMPOSITOR = {
    "cosmic-comp", "mutter", "kwin_wayland", "kwin_x11", "xfwm4",
    "openbox", "i3", "sway", "xorg", "xwayland", "gnome-shell",
    "plasmashell", "xdg-desktop-portal-cosmic",
}
# Terminal emulators — never suspend, they hold user shells and agents
_TERMINAL_EMULATORS = {
    "cosmic-term", "gnome-terminal", "gnome-terminal-server",
    "tilix", "xterm", "alacritty", "kitty", "konsole",
    "xfce4-terminal", "lxterminal", "mate-terminal", "terminator",
}
_SYSTEM = {
    "systemd", "kernel", "kthreadd", "ksoftirqd", "kworker",
    "pulseaudio", "pipewire", "wireplumber", "dbus-daemon",
    "networkmanager", "wpa_supplicant", "avahi-daemon", "polkitd",
    "xdg-desktop-portal", "xdg-document-portal", "xdg-permission-store",
    "gvfsd", "gvfs-udisks2-volume-monitor", "udisksd",
    "upowerd", "bluetoothd", "cups", "cupsd",
    "at-spi-bus-launcher", "at-spi2-registryd",
    "gnome-keyring-daemon", "seahorse",
    "systemd-resolved", "systemd-udevd", "snapd",
    "tailscaled", "dnsmasq", "containerd", "dockerd",
}
_KNOWN_AGENTS = {
    "claude",        # Claude Code
    "codex",
    "ollama",
}
_WORK_TOOLS = {
    "burpsuite", "burpsuitecommunity", "java",  # java = burpsuite on this machine
    "wireshark", "zap", "owasp-zap",
    "antigravity",   # user's own tool
    "openclaw-gateway",  # appears on this machine, treat as work
}
_BROWSER_PROCS = {
    "brave", "brave-browser", "chrome", "chromium", "chromium-browser",
    "google-chrome", "firefox", "firefox-bin",
    "isolated web co",  # Chrome/Brave renderer label
}

# RSS thresholds
_IDLE_RSS_THRESHOLD_MB = 150   # only flag processes using > this much
_ACTIVE_CPU_THRESHOLD  = 0.5   # CPU % above this = "active" in recent window


@dataclass
class ProcessSample:
    rss_mb: float
    cpu_pct: float
    timestamp: float = field(default_factory=time.monotonic)


@dataclass
class ProcessProfile:
    pid: int
    name: str
    category: Category
    rss_mb: float
    status: str
    uid: int = 0
    samples: deque = field(default_factory=lambda: deque(maxlen=60))
    # Derived
    peak_cpu_recent: float = 0.0
    suspend_candidate: bool = False
    reason: str = ""

    def update(self, rss_mb: float, cpu_pct: float):
        self.rss_mb = rss_mb
        self.samples.append(ProcessSample(rss_mb=rss_mb, cpu_pct=cpu_pct))
        if self.samples:
            self.peak_cpu_recent = max(s.cpu_pct for s in self.samples)
        self._reclassify()

    def _reclassify(self):
        self.suspend_candidate = (
            self.category == "USER_IDLE"
            and self.rss_mb >= _IDLE_RSS_THRESHOLD_MB
            and self.status not in ("stopped",)
        )
        if self.suspend_candidate:
            self.reason = (
                f"{self.rss_mb:.0f} MB RSS, 0% CPU for {len(self.samples) * 10 // 60} min"
            )


class ProcessProfiler:
    """
    Singleton that polls processes every ~10 seconds and maintains profiles.
    Call `tick()` from the monitor loop; call `snapshot()` to get current state.
    """

    def __init__(self):
        self._profiles: dict[int, ProcessProfile] = {}
        self._our_pid = os.getpid()

    def tick(self):
        """Sample all processes. Call every monitor poll cycle."""
        seen_pids: set[int] = set()
        for proc in psutil.process_iter(["pid", "name", "memory_info", "status", "uids", "cpu_percent"]):
            try:
                pid = proc.info["pid"]
                if pid == self._our_pid:
                    continue
                seen_pids.add(pid)
                name = (proc.info["name"] or "").strip()
                rss_mb = (proc.info["memory_info"].rss or 0) / (1024 * 1024)
                status = proc.info["status"] or "unknown"
                cpu = proc.info.get("cpu_percent") or 0.0
                try:
                    uid = proc.uids().real
                except Exception:
                    uid = 0

                if pid not in self._profiles:
                    cat = _classify(name, uid)
                    self._profiles[pid] = ProcessProfile(
                        pid=pid, name=name, category=cat,
                        rss_mb=rss_mb, status=status, uid=uid,
                    )
                else:
                    self._profiles[pid].status = status
                    self._profiles[pid].rss_mb = rss_mb

                self._profiles[pid].update(rss_mb, cpu)
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                continue

        # Remove dead processes
        for dead_pid in set(self._profiles) - seen_pids:
            self._profiles.pop(dead_pid, None)

    def snapshot(self) -> list[ProcessProfile]:
        return sorted(self._profiles.values(), key=lambda p: p.rss_mb, reverse=True)

    def candidates(self) -> list[ProcessProfile]:
        """Processes worth considering for suspension."""
        return [p for p in self._profiles.values() if p.suspend_candidate]

    def report(self) -> dict:
        """Summary report for /stats and Claude."""
        profs = self.snapshot()
        by_cat: dict[str, list[dict]] = {}
        for p in profs:
            by_cat.setdefault(p.category, []).append({
                "pid": p.pid, "name": p.name,
                "rss_mb": round(p.rss_mb, 1),
                "peak_cpu": round(p.peak_cpu_recent, 1),
                "status": p.status,
                "suspend_candidate": p.suspend_candidate,
                "reason": p.reason,
            })
        return {
            "by_category": by_cat,
            "candidates": [
                {"pid": p.pid, "name": p.name, "rss_mb": round(p.rss_mb, 1), "reason": p.reason}
                for p in self.candidates()
            ],
            "total_profiled": len(profs),
        }


# ── Singleton instance ────────────────────────────────────────────────────────
_profiler = ProcessProfiler()


def tick():
    _profiler.tick()


def get_profiler() -> ProcessProfiler:
    return _profiler


# ── Classification helper ─────────────────────────────────────────────────────

def _classify(name: str, uid: int) -> Category:
    lower = name.lower()

    if lower in _COMPOSITOR:
        return "COMPOSITOR"
    if lower in _TERMINAL_EMULATORS:
        return "SYSTEM"   # treat terminals like system — never suspend
    if lower in _SYSTEM or uid == 0:
        return "SYSTEM"
    if any(a in lower for a in _KNOWN_AGENTS):
        return "AGENT"
    if any(w in lower for w in _WORK_TOOLS):
        return "WORK_TOOL"
    if any(b in lower for b in _BROWSER_PROCS):
        return "BROWSER"

    return "USER_IDLE"
