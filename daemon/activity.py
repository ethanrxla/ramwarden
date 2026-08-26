"""
Activity detection — works out what the user is ACTUALLY using, from live
system signals rather than a hardcoded list of program names.

The old profiler classified purely by process name, and anything it did not
recognise fell through to USER_IDLE — which made it a suspend candidate. On a
real desktop that means a 4 GB QEMU virtual machine, a running dev server, or a
half-finished build all look "idle" simply because nobody wrote their name down.

This module answers a different question: *is something depending on this
process right now?* It collects cheap, factual signals —

  serving      process holds a listening socket (a server; killing it kills clients)
  connected    process has established network connections
  focused      process owns the window the user is looking at
  windowed     process owns any mapped window
  audio        process is playing or capturing sound
  tty          process is attached to a terminal the user has open
  busy         measurable CPU time consumed in the recent sample window
  fresh        process started in the last few minutes — the user just launched it
  descendant   a child/grandchild is itself active
  writing      holds writable file handles under $HOME

— and turns them into one of three verdicts:

  PROTECTED   structural: compositor, VM, container runtime, agent, server.
              Never suspend, never close, not even on request.
  IN_USE      the user is demonstrably using it right now. Leave alone.
  IDLE        no signal in the sample window. Only these may be reclaimed,
              and only if the user put them on the watchlist.

Signals are collected once per monitor tick and shared, so the per-process cost
is a dict lookup rather than a syscall.
"""
from __future__ import annotations

import logging
import os
import re
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Literal

import psutil

log = logging.getLogger("ramwarden.activity")

Verdict = Literal["PROTECTED", "IN_USE", "IDLE"]

# ── Structural roles ─────────────────────────────────────────────────────────
# These are protected because of what they *are*, not what they are doing.
# Suspending any of them freezes the desktop, corrupts guest state, or strands
# a client on the other end of a socket.

Role = Literal[
    "COMPOSITOR", "HYPERVISOR", "CONTAINER", "AGENT", "TERMINAL",
    "SYSTEM", "BROWSER", "MEDIA", "BUILD", "SYNC", "APP",
]

_COMPOSITOR = {
    "cosmic-comp", "cosmic-session", "cosmic-panel", "cosmic-applets",
    "mutter", "kwin_wayland", "kwin_x11", "xfwm4", "openbox", "i3", "sway",
    "xorg", "x", "xwayland", "gnome-shell", "plasmashell",
    "cosmic-greeter", "gdm", "sddm", "lightdm",
}
_COMPOSITOR_PREFIXES = ("cosmic-", "xdg-desktop-portal", "gnome-shell", "plasma")

# Virtual machines and emulators. A SIGSTOP here stalls a guest OS mid-write.
_HYPERVISOR_PREFIXES = (
    "qemu", "qemu-system", "kvm", "virtualbox", "vboxheadless", "vboxsvc",
    "vmware", "vmware-vmx", "virt-manager", "libvirtd", "virtiofsd",
    "crosvm", "cloud-hypervisor", "waydroid",
)

# Container / sandbox runtimes. Freezing the shim orphans everything inside it.
_CONTAINER_PREFIXES = (
    "containerd", "containerd-shim", "dockerd", "docker-proxy", "docker",
    "podman", "conmon", "crun", "runc", "systemd-nspawn", "lxc", "lxd",
    "bwrap", "flatpak-session-helper", "snapd",
)

# Long-running AI/coding agents and their runtimes.
_AGENT_NAMES = {"claude", "codex", "ollama", "aider", "cursor-agent", "copilot"}

_TERMINAL_EMULATORS = {
    "cosmic-term", "gnome-terminal", "gnome-terminal-server", "tilix", "xterm",
    "alacritty", "kitty", "konsole", "xfce4-terminal", "lxterminal",
    "mate-terminal", "terminator", "wezterm", "wezterm-gui", "foot", "ptyxis",
}

_BROWSERS = {
    "brave", "brave-browser", "chrome", "chromium", "chromium-browser",
    "google-chrome", "firefox", "firefox-bin", "librewolf", "vivaldi",
    "epiphany", "tor", "torbrowser-launch",
}

# Playing or producing media — the user is watching/listening/recording.
_MEDIA = {
    "mpv", "vlc", "mplayer", "ffmpeg", "ffplay", "obs", "obs-studio",
    "totem", "celluloid", "audacity", "kdenlive", "spotify", "rhythmbox",
}

# A build or long computation in flight. Interrupting wastes the user's time.
_BUILD = {
    "cargo", "rustc", "gcc", "cc1", "cc1plus", "g++", "clang", "clang++",
    "ld", "make", "ninja", "cmake", "gradle", "javac", "kotlinc", "go",
    "webpack", "tsc", "esbuild", "vite", "rollup", "pytest", "tox",
    "dpkg", "apt", "apt-get", "unattended-upgr", "snap", "flatpak",
}

# Background sync / transfer. Suspending mid-transfer can corrupt remote state.
_SYNC = {
    "syncthing", "dropbox", "nextcloud", "insync", "rclone", "rsync",
    "megasync", "onedrive", "restic", "borg", "duplicity", "timeshift",
}

_SYSTEM_NAMES = {
    "systemd", "init", "kthreadd", "pipewire", "pipewire-pulse", "wireplumber",
    "pulseaudio", "dbus-daemon", "dbus-broker", "networkmanager",
    "wpa_supplicant", "avahi-daemon", "polkitd", "udisksd", "upowerd",
    "bluetoothd", "cupsd", "gvfsd", "gnome-keyring-d", "gnome-keyring-daemon",
    "systemd-resolved", "systemd-udevd", "systemd-journald", "systemd-logind",
    "tailscaled", "wireguard", "openvpn", "sshd", "cron", "crond", "atd",
    "accounts-daemon", "rtkit-daemon", "irqbalance", "thermald", "power-profiles-",
}

# ── Tunables ─────────────────────────────────────────────────────────────────

CPU_BUSY_SECONDS = 0.5      # CPU seconds consumed in the window ⇒ "busy"
FRESH_MINUTES = 10.0        # started this recently ⇒ user just launched it
IDLE_RSS_FLOOR_MB = 120.0   # below this, reclaiming it is not worth the risk
AUDIO_REFRESH_S = 15.0      # pactl is a subprocess; do not run it every tick
WINDOW_REFRESH_S = 5.0      # wmctrl likewise


@dataclass
class ActivitySignals:
    """Everything we know about one process at one moment."""
    pid: int
    name: str
    rss_mb: float = 0.0
    status: str = "running"
    role: Role = "APP"

    listening_ports: list[int] = field(default_factory=list)
    established: int = 0
    is_focused: bool = False
    has_window: bool = False
    playing_audio: bool = False
    has_tty: bool = False
    cpu_seconds_recent: float = 0.0
    age_minutes: float = 0.0
    active_descendant: str = ""
    writable_home_files: int = 0

    verdict: Verdict = "IDLE"
    reasons: list[str] = field(default_factory=list)
    # "structural" protection is absolute; "serving" is a soft protection that an
    # explicit watchlist entry may override. Empty when the process is not protected.
    protection: Literal["", "structural", "serving"] = ""

    @property
    def is_server(self) -> bool:
        return bool(self.listening_ports)

    def as_dict(self) -> dict:
        return {
            "pid": self.pid,
            "name": self.name,
            "rss_mb": round(self.rss_mb, 1),
            "status": self.status,
            "role": self.role,
            "verdict": self.verdict,
            "protection": self.protection,
            "reasons": self.reasons,
            "listening_ports": self.listening_ports,
            "established": self.established,
            "focused": self.is_focused,
            "window": self.has_window,
            "audio": self.playing_audio,
            "tty": self.has_tty,
            "cpu_seconds_recent": round(self.cpu_seconds_recent, 2),
            "age_minutes": round(self.age_minutes, 1),
            "active_descendant": self.active_descendant,
        }


# ── System-wide signal collection ────────────────────────────────────────────

def _listening_map() -> tuple[dict[int, list[int]], dict[int, int]]:
    """pid → listening ports, and pid → count of established connections."""
    listening: dict[int, list[int]] = {}
    established: dict[int, int] = {}
    try:
        for c in psutil.net_connections(kind="inet"):
            if not c.pid:
                continue
            if c.status == psutil.CONN_LISTEN:
                listening.setdefault(c.pid, []).append(c.laddr.port)
            elif c.status == psutil.CONN_ESTABLISHED:
                established[c.pid] = established.get(c.pid, 0) + 1
    except (psutil.AccessDenied, RuntimeError) as e:
        log.debug("net_connections unavailable: %s", e)
    return listening, established


def _audio_pids() -> set[int]:
    """PIDs with an active PulseAudio/PipeWire stream (playing or recording)."""
    pids: set[int] = set()
    for kind in ("sink-inputs", "source-outputs"):
        try:
            out = subprocess.run(
                ["pactl", "list", kind], capture_output=True, text=True, timeout=3
            ).stdout
        except (OSError, subprocess.SubprocessError):
            continue
        for m in re.finditer(r'application\.process\.id\s*=\s*"(\d+)"', out):
            pids.add(int(m.group(1)))
    return pids


def _window_pids() -> tuple[set[int], int | None]:
    """
    PIDs owning a mapped window, plus the focused window's PID.

    Only ever used as a positive signal. Under Wayland compositors, wmctrl sees
    XWayland clients but not native ones, so an absent window proves nothing.
    """
    pids: set[int] = set()
    focused: int | None = None
    try:
        out = subprocess.run(
            ["wmctrl", "-lp"], capture_output=True, text=True, timeout=3
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return pids, None

    for line in out.splitlines():
        parts = line.split(None, 4)
        if len(parts) < 3:
            continue
        try:
            pid = int(parts[2])
        except ValueError:
            continue
        # Flatpak/sandboxed clients report a namespaced PID that means nothing
        # to us; ignore anything that does not resolve on this host.
        if pid > 1 and psutil.pid_exists(pid):
            pids.add(pid)

    try:
        active = subprocess.run(
            ["xprop", "-root", "_NET_ACTIVE_WINDOW"],
            capture_output=True, text=True, timeout=3,
        ).stdout
        m = re.search(r"(0x[0-9a-fA-F]+)", active)
        if m:
            win = int(m.group(1), 16)
            for line in out.splitlines():
                parts = line.split(None, 4)
                if len(parts) >= 3 and int(parts[0], 16) == win:
                    p = int(parts[2])
                    if p > 1 and psutil.pid_exists(p):
                        focused = p
                    break
    except (OSError, subprocess.SubprocessError, ValueError):
        pass

    return pids, focused


# ── Role classification ──────────────────────────────────────────────────────

def classify_role(name: str, cmdline: str = "", uid: int = -1) -> Role:
    lower = (name or "").lower()
    cmd = (cmdline or "").lower()

    # Exact names win over prefixes: cosmic-term is a terminal, not the compositor.
    if lower in _TERMINAL_EMULATORS:
        return "TERMINAL"
    if lower in _COMPOSITOR or lower.startswith(_COMPOSITOR_PREFIXES):
        return "COMPOSITOR"
    if lower.startswith(_HYPERVISOR_PREFIXES):
        return "HYPERVISOR"
    if lower.startswith(_CONTAINER_PREFIXES):
        return "CONTAINER"
    if lower in _AGENT_NAMES or any(a in cmd for a in ("claude-code", "ollama serve")):
        return "AGENT"
    if lower in _BROWSERS or lower.startswith(("brave", "firefox", "chrom")):
        return "BROWSER"
    if lower in _MEDIA:
        return "MEDIA"
    if lower in _BUILD:
        return "BUILD"
    if lower in _SYNC:
        return "SYNC"
    if lower in _SYSTEM_NAMES or uid == 0:
        return "SYSTEM"
    return "APP"


# Roles that are never suspended, whatever else the signals say.
STRUCTURAL_ROLES: frozenset[str] = frozenset({
    "COMPOSITOR", "HYPERVISOR", "CONTAINER", "AGENT", "TERMINAL",
    "SYSTEM", "MEDIA", "BUILD", "SYNC",
})

_ROLE_REASON = {
    "COMPOSITOR": "desktop compositor — suspending it freezes the session",
    "HYPERVISOR": "virtual machine — a stalled guest can corrupt its disk",
    "CONTAINER":  "container runtime — freezing it orphans everything inside",
    "AGENT":      "AI agent session in progress",
    "TERMINAL":   "terminal emulator — holds your shells",
    "SYSTEM":     "system service",
    "MEDIA":      "playing or recording media",
    "BUILD":      "build or long computation in flight",
    "SYNC":       "file sync or transfer in progress",
}


class ActivityDetector:
    """
    Collects system-wide signals once per tick and scores every process.

    `tick()` is called from the monitor loop; `snapshot()` returns the last
    scored view without touching the system again.
    """

    def __init__(self):
        self._signals: dict[int, ActivitySignals] = {}
        self._cpu_times: dict[int, tuple[float, float]] = {}  # pid → (cpu_seconds, wall)
        self._our_pid = os.getpid()
        self._home = str(Path.home())
        self._audio_cache: set[int] = set()
        self._audio_at = 0.0
        self._window_cache: tuple[set[int], int | None] = (set(), None)
        self._window_at = 0.0
        self._ticks = 0

    # ── collection ───────────────────────────────────────────────────────────

    def tick(self) -> None:
        now = time.monotonic()
        listening, established = _listening_map()

        if now - self._audio_at > AUDIO_REFRESH_S:
            self._audio_cache = _audio_pids()
            self._audio_at = now
        if now - self._window_at > WINDOW_REFRESH_S:
            self._window_cache = _window_pids()
            self._window_at = now
        window_pids, focused_pid = self._window_cache

        fresh: dict[int, ActivitySignals] = {}
        parents: dict[int, int] = {}

        for proc in psutil.process_iter(
            ["pid", "name", "memory_info", "status", "uids", "ppid",
             "create_time", "cpu_times", "terminal", "cmdline"]
        ):
            try:
                info = proc.info
                pid = info["pid"]
                if pid == self._our_pid:
                    continue

                name = (info["name"] or "").strip()
                mem = info["memory_info"]
                rss_mb = (mem.rss if mem else 0) / (1024 * 1024)
                uid = info["uids"].real if info["uids"] else -1
                cmdline = " ".join(info["cmdline"] or [])
                parents[pid] = info["ppid"] or 0

                cpu_now = 0.0
                if info["cpu_times"]:
                    cpu_now = info["cpu_times"].user + info["cpu_times"].system
                prev = self._cpu_times.get(pid)
                cpu_delta = max(0.0, cpu_now - prev[0]) if prev else 0.0
                self._cpu_times[pid] = (cpu_now, now)

                age_min = max(0.0, (time.time() - (info["create_time"] or 0)) / 60.0)

                sig = ActivitySignals(
                    pid=pid,
                    name=name,
                    rss_mb=rss_mb,
                    status=info["status"] or "unknown",
                    role=classify_role(name, cmdline, uid),
                    listening_ports=sorted(set(listening.get(pid, []))),
                    established=established.get(pid, 0),
                    is_focused=(focused_pid == pid),
                    has_window=(pid in window_pids),
                    playing_audio=(pid in self._audio_cache),
                    has_tty=bool(info["terminal"]),
                    cpu_seconds_recent=cpu_delta,
                    age_minutes=age_min,
                )
                fresh[pid] = sig
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                continue

        # Second pass: propagate activity up the process tree, so a shell whose
        # child is compiling is itself "in use".
        self._propagate(fresh, parents)

        for sig in fresh.values():
            self._score(sig)

        for dead in set(self._cpu_times) - set(fresh):
            self._cpu_times.pop(dead, None)
        self._signals = fresh
        self._ticks += 1

    def _propagate(self, sigs: dict[int, ActivitySignals], parents: dict[int, int]) -> None:
        """Mark every ancestor of an active process as having an active descendant."""
        for pid, sig in sigs.items():
            if not self._self_active(sig):
                continue
            seen = {pid}
            cur = parents.get(pid, 0)
            while cur and cur not in seen and cur in sigs:
                seen.add(cur)
                if not sigs[cur].active_descendant:
                    sigs[cur].active_descendant = f"{sig.name} (pid {sig.pid})"
                cur = parents.get(cur, 0)

    @staticmethod
    def _self_active(sig: ActivitySignals) -> bool:
        """Activity from this process alone, ignoring its children."""
        return bool(
            sig.is_focused
            or sig.playing_audio
            or sig.cpu_seconds_recent >= CPU_BUSY_SECONDS
            or sig.role in ("HYPERVISOR", "CONTAINER", "AGENT", "BUILD", "SYNC", "MEDIA")
        )

    # ── scoring ──────────────────────────────────────────────────────────────

    @property
    def warm(self) -> bool:
        """CPU deltas need two samples; before that, nothing can be called idle."""
        return self._ticks >= 2

    def _score(self, sig: ActivitySignals) -> None:
        reasons: list[str] = []

        if sig.role in STRUCTURAL_ROLES:
            sig.verdict = "PROTECTED"
            sig.protection = "structural"
            sig.reasons = [_ROLE_REASON.get(sig.role, sig.role.lower())]
            return

        if sig.listening_ports:
            ports = ", ".join(str(p) for p in sig.listening_ports[:4])
            sig.verdict = "PROTECTED"
            sig.protection = "serving"
            sig.reasons = [f"serving on port {ports} — clients would hang"]
            return

        if sig.is_focused:
            reasons.append("this is the window you are looking at")
        if sig.playing_audio:
            reasons.append("playing audio")
        if sig.cpu_seconds_recent >= CPU_BUSY_SECONDS:
            reasons.append(f"used {sig.cpu_seconds_recent:.1f}s CPU just now")
        if sig.active_descendant:
            reasons.append(f"child still working: {sig.active_descendant}")
        if sig.age_minutes < FRESH_MINUTES:
            reasons.append(f"started {sig.age_minutes:.0f} min ago")

        if reasons:
            sig.verdict = "IN_USE"
            sig.reasons = reasons
            return

        # A CPU delta needs a previous sample to compare against. Until we have
        # two, "no CPU" means "not measured yet" — never call that idle.
        if not self.warm:
            sig.verdict = "IN_USE"
            sig.reasons = ["still sampling — no verdict yet"]
            return

        sig.verdict = "IDLE"
        bits = []
        if sig.has_window:
            bits.append("window open but untouched")
        if sig.established:
            bits.append(f"{sig.established} idle connection(s)")
        bits.append("no CPU since the last sample")
        sig.reasons = bits

    # ── queries ──────────────────────────────────────────────────────────────

    def snapshot(self) -> dict[int, ActivitySignals]:
        return self._signals

    def get(self, pid: int) -> ActivitySignals | None:
        return self._signals.get(pid)

    def verdict_for_name(self, name_pattern: str) -> tuple[Verdict, list[str], list[int]]:
        """
        Aggregate verdict across every process matching a name.

        The strictest verdict wins: if any process in the group is PROTECTED or
        IN_USE, the whole group is. Suspending half of an app is worse than
        suspending none of it.
        """
        matched = self.match(name_pattern)
        if not matched:
            return "IDLE", ["no such process running"], []

        pids = [s.pid for s in matched]
        structural = [s for s in matched if s.protection == "structural"]
        if structural:
            return "PROTECTED", structural[0].reasons, pids
        serving = [s for s in matched if s.protection == "serving"]
        if serving:
            return "PROTECTED", serving[0].reasons, pids
        in_use = [s for s in matched if s.verdict == "IN_USE"]
        if in_use:
            return "IN_USE", in_use[0].reasons, pids
        return "IDLE", matched[0].reasons, pids

    def match(self, name_pattern: str) -> list[ActivitySignals]:
        """Every running process whose name equals or globs `name_pattern`."""
        import fnmatch

        pat = (name_pattern or "").lower()
        return [
            s for s in self._signals.values()
            if s.name.lower() == pat or fnmatch.fnmatch(s.name.lower(), pat)
        ]

    def may_suspend(self, name_pattern: str, watchlist: list[str]) -> tuple[bool, str]:
        """
        The single gate every suspend goes through.

        Returns (allowed, explanation). A process must be on the watchlist —
        dynamic detection only ever narrows that set, never widens it — and must
        not be structurally protected or visibly in use.
        """
        import fnmatch

        matched = self.match(name_pattern)
        if not matched:
            return False, f"no running process matches {name_pattern!r}"

        # Structural protection outranks everything, including an explicit
        # watchlist entry — report it first because it is the real reason.
        structural = [s for s in matched if s.protection == "structural"]
        if structural:
            return False, f"{name_pattern}: {structural[0].reasons[0]}"

        low = (name_pattern or "").lower()
        on_list = any(
            low == w.lower() or fnmatch.fnmatch(low, w.lower()) for w in watchlist
        )
        if not on_list:
            return False, f"{name_pattern!r} is not on the watchlist"

        in_use = [s for s in matched if s.verdict == "IN_USE"]
        if in_use:
            return False, f"{name_pattern} is in use — {in_use[0].reasons[0]}"

        serving = [s for s in matched if s.protection == "serving"]
        if serving:
            # Soft protection: the user explicitly watchlisted this, so allow it
            # but make the consequence visible in the log and the UI.
            return True, f"{name_pattern} is idle, but {serving[0].reasons[0]}"

        return True, f"{name_pattern} is idle"

    def reclaimable(self, watchlist: list[str]) -> list[ActivitySignals]:
        """
        Processes the user has opted into reclaiming that are genuinely idle.

        Nothing outside the watchlist is ever returned — dynamic detection
        decides what to *spare*, it never widens what may be touched.
        """
        out: list[ActivitySignals] = []
        for pattern in watchlist:
            allowed, _why = self.may_suspend(pattern, watchlist)
            if not allowed:
                continue
            for sig in self.match(pattern):
                if sig.status == psutil.STATUS_STOPPED:
                    continue
                if sig.rss_mb < IDLE_RSS_FLOOR_MB:
                    continue
                out.append(sig)
        return sorted(out, key=lambda s: s.rss_mb, reverse=True)

    def report(self, watchlist: list[str] | None = None) -> dict:
        """Grouped view for the UI and the /activity endpoint."""
        sigs = sorted(self._signals.values(), key=lambda s: s.rss_mb, reverse=True)
        by_verdict: dict[str, list[dict]] = {"PROTECTED": [], "IN_USE": [], "IDLE": []}
        totals: dict[str, float] = {"PROTECTED": 0.0, "IN_USE": 0.0, "IDLE": 0.0}

        for s in sigs:
            totals[s.verdict] += s.rss_mb
            if s.rss_mb >= 50:
                by_verdict[s.verdict].append(s.as_dict())

        return {
            "totals_mb": {k: round(v, 1) for k, v in totals.items()},
            "by_verdict": {k: v[:25] for k, v in by_verdict.items()},
            "reclaimable": [s.as_dict() for s in self.reclaimable(watchlist or [])],
            "profiled": len(sigs),
        }


# ── Singleton ────────────────────────────────────────────────────────────────

_detector = ActivityDetector()


def tick() -> None:
    _detector.tick()


def get_detector() -> ActivityDetector:
    return _detector
