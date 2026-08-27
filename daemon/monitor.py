import logging
import time
from dataclasses import dataclass

import psutil

from . import config

log = logging.getLogger("ramwarden.monitor")

# Chromium renderer processes all appear under these names
_BRAVE_NAMES = {"brave", "brave-browser"}
_FIREFOX_NAMES = {"firefox", "firefox-bin"}
_CHROMIUM_NAMES = {"chromium", "chromium-browser", "google-chrome"}


@dataclass
class ProcessInfo:
    name: str
    pid: int
    rss_mb: float
    status: str = "running"  # psutil status: running, stopped, sleeping, etc.


@dataclass
class RAMSnapshot:
    total_mb: float
    used_mb: float
    percent: float
    processes: list[ProcessInfo]


def snapshot() -> RAMSnapshot:
    vm = psutil.virtual_memory()
    procs: dict[str, float] = {}
    proc_pids: dict[str, int] = {}

    proc_statuses: dict[str, str] = {}

    for p in psutil.process_iter(["pid", "name", "memory_info", "status"]):
        try:
            name = p.info["name"] or ""
            rss = (p.info["memory_info"].rss or 0) / (1024 * 1024)
            status = p.info.get("status", "running")
            bucket = _bucket(name)
            procs[bucket] = procs.get(bucket, 0.0) + rss
            if bucket not in proc_pids:
                proc_pids[bucket] = p.info["pid"]
                proc_statuses[bucket] = status
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            continue

    top = sorted(
        [
            ProcessInfo(name=k, pid=proc_pids[k], rss_mb=round(v, 1), status=proc_statuses.get(k, "running"))
            for k, v in procs.items()
        ],
        key=lambda p: p.rss_mb,
        reverse=True,
    )[:15]

    return RAMSnapshot(
        total_mb=round(vm.total / (1024 * 1024), 1),
        used_mb=round(vm.used / (1024 * 1024), 1),
        percent=vm.percent,
        processes=top,
    )


def _bucket(name: str) -> str:
    lower = name.lower()
    if lower in _BRAVE_NAMES or lower.startswith("brave"):
        return "Brave"
    if lower in _FIREFOX_NAMES:
        return "Firefox"
    if lower in _CHROMIUM_NAMES:
        return "Chromium"
    return name


class Monitor:
    """Polls RAM on an interval and calls on_threshold when pressure is high."""

    def __init__(self, on_threshold, poll_interval: float = 10.0, on_resume=None):
        self._on_threshold = on_threshold
        self._on_resume = on_resume
        self._poll_interval = poll_interval
        self._last_trigger: float = 0.0
        self._running = False

    def start(self):
        self._running = True
        while self._running:
            self._tick()
            time.sleep(self._poll_interval)

    def stop(self):
        self._running = False

    def _tick(self):
        from .process_profiler import tick as profiler_tick
        from .activity import tick as activity_tick
        profiler_tick()   # always sample — builds CPU history
        activity_tick()   # always sample — builds the "what is in use" picture

        cfg = config.get()
        snap = snapshot()

        if snap.percent < cfg.thresholds.ram_percent:
            # Pressure is off. Anything RamWarden froze was frozen to buy headroom
            # we no longer need, so give it back — a suspended GUI app is
            # indistinguishable from a crashed one, and leaving it stopped once the
            # reason has passed is how a user ends up force-quitting a healthy app.
            self._auto_resume(snap)
            return
        now = time.monotonic()
        debounce_secs = cfg.thresholds.debounce_minutes * 60
        if now - self._last_trigger < debounce_secs:
            return
        self._last_trigger = now
        self._on_threshold(snap)

    def _auto_resume(self, snap):
        """Wake anything RamWarden suspended, now that memory pressure has eased."""
        from .process_manager import ProcessManager, suspended_entries

        entries = suspended_entries()
        if not entries:
            return

        pm = ProcessManager()
        for entry in entries:
            resumed = pm.resume_entry(entry)
            log.info(
                "Auto-resumed %s (%d process(es)) — RAM back to %.0f%% after %.0f min frozen",
                entry.name, len(resumed), snap.percent, entry.minutes,
            )
            if self._on_resume:
                self._on_resume(entry.name, len(resumed))
