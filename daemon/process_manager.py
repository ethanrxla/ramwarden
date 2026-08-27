import fnmatch
import logging
import signal
import threading
import time
from dataclasses import dataclass, field

import psutil

from . import config

log = logging.getLogger("ramwarden.process_manager")


class SuspendRefused(Exception):
    """Raised when a suspend is blocked by the watchlist or an activity signal."""


@dataclass
class SuspendedEntry:
    """One app RamWarden froze, and when."""
    name: str
    pids: list[int]
    since: float = field(default_factory=time.time)

    @property
    def minutes(self) -> float:
        return max(0.0, (time.time() - self.since) / 60.0)

    def live_pids(self) -> list[int]:
        """PIDs still stopped. A process the user killed or continued drops out."""
        alive = []
        for pid in self.pids:
            try:
                if psutil.Process(pid).status() == psutil.STATUS_STOPPED:
                    alive.append(pid)
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                continue
        return alive

    def rss_mb(self) -> float:
        total = 0.0
        for pid in self.live_pids():
            try:
                total += psutil.Process(pid).memory_info().rss / (1024 * 1024)
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                continue
        return total


# Only what RamWarden froze. A process the user stopped themselves (Ctrl-Z, or a
# debugger) must never be swept up by auto-resume, so membership here — not the
# STOPPED status — is what makes something ours to wake.
_suspended: dict[str, SuspendedEntry] = {}
_registry_lock = threading.Lock()


def suspended_entries() -> list[SuspendedEntry]:
    """Currently-frozen apps, pruned of anything that has since died or woken."""
    with _registry_lock:
        for name in list(_suspended):
            if not _suspended[name].live_pids():
                log.info("%s is no longer stopped — dropping it from the registry", name)
                _suspended.pop(name, None)
        return sorted(_suspended.values(), key=lambda e: e.since)


def forget(name: str) -> None:
    with _registry_lock:
        _suspended.pop(name, None)


class ProcessManager:
    def suspend(self, name_pattern: str, force: bool = False) -> list[int]:
        """
        Send SIGSTOP to all processes matching name_pattern.

        Every suspend goes through the activity gate first: the target must be on
        the configured watchlist and must not be structurally protected (VM,
        container runtime, compositor, agent, system service) or visibly in use.
        The gate is what stops a hallucinated or fat-fingered name from freezing a
        4 GB virtual machine. `force=True` skips only the watchlist and in-use
        checks — structural protection is never bypassable.
        """
        allowed, why = self._gate(name_pattern, force=force)
        if not allowed:
            log.warning("Refusing to suspend %s — %s", name_pattern, why)
            raise SuspendRefused(why)
        log.info("Suspend allowed for %s — %s", name_pattern, why)

        pids = self._find(name_pattern)
        suspended = []
        for pid in pids:
            try:
                proc = psutil.Process(pid)
                if proc.status() == psutil.STATUS_STOPPED:
                    log.info("Skipping %s (PID %d) — already stopped", proc.name(), pid)
                    continue
                proc.suspend()
                log.info("Suspended %s (PID %d)", proc.name(), pid)
                suspended.append(pid)
            except (psutil.NoSuchProcess, psutil.AccessDenied) as e:
                log.warning("Could not suspend PID %d: %s", pid, e)

        if suspended:
            with _registry_lock:
                _suspended[name_pattern] = SuspendedEntry(name=name_pattern, pids=suspended)
        return suspended

    def resume(self, name_pattern: str) -> list[int]:
        """Send SIGCONT to all processes matching name_pattern. Returns resumed PIDs."""
        pids = self._find(name_pattern)
        resumed = []
        for pid in pids:
            try:
                proc = psutil.Process(pid)
                proc.resume()
                log.info("Resumed %s (PID %d)", proc.name(), pid)
                resumed.append(pid)
            except (psutil.NoSuchProcess, psutil.AccessDenied) as e:
                log.warning("Could not resume PID %d: %s", pid, e)

        forget(name_pattern)
        return resumed

    def resume_pids(self, pids: list[int]) -> list[int]:
        """
        SIGCONT specific PIDs. Resuming by name would also touch a newly launched
        instance of the same app — after a frozen app gets force-quit and relaunched,
        the name matches two different process trees.
        """
        resumed = []
        for pid in pids:
            try:
                psutil.Process(pid).resume()
                resumed.append(pid)
            except (psutil.NoSuchProcess, psutil.AccessDenied) as e:
                log.warning("Could not resume PID %d: %s", pid, e)
        return resumed

    def resume_entry(self, entry: "SuspendedEntry") -> list[int]:
        """Wake exactly the processes RamWarden froze under this entry."""
        resumed = self.resume_pids(entry.live_pids())
        forget(entry.name)
        return resumed

    def kill(self, name_pattern: str) -> list[int]:
        """SIGTERM then SIGKILL processes matching name_pattern. Returns killed PIDs."""
        pids = self._find(name_pattern)
        killed = []
        for pid in pids:
            try:
                proc = psutil.Process(pid)
                proc.terminate()
                try:
                    proc.wait(timeout=3)
                except psutil.TimeoutExpired:
                    proc.kill()
                log.info("Killed %s (PID %d)", proc.name(), pid)
                killed.append(pid)
            except (psutil.NoSuchProcess, psutil.AccessDenied) as e:
                log.warning("Could not kill PID %d: %s", pid, e)
        return killed

    @staticmethod
    def _gate(name_pattern: str, force: bool = False) -> tuple[bool, str]:
        """Consult the activity detector. Fails closed if it has no data yet."""
        from .activity import get_detector

        det = get_detector()
        if not det.warm:
            # Fewer than two samples: no CPU deltas exist yet, so nothing can be
            # honestly called idle. Refuse rather than guess.
            return False, "activity detector is still warming up — try again in ~20s"
        if not det.snapshot():
            # No samples yet (daemon just started). Refuse rather than guess.
            return False, "activity detector has no samples yet — try again in a few seconds"

        if force:
            structural = [
                s for s in det.match(name_pattern) if s.protection == "structural"
            ]
            if structural:
                return False, f"{name_pattern}: {structural[0].reasons[0]} (not forceable)"
            return True, f"forced by user for {name_pattern}"

        return det.may_suspend(name_pattern, config.get().watchlist)

    def _find(self, name_pattern: str) -> list[int]:
        pids = []
        for proc in psutil.process_iter(["pid", "name"]):
            try:
                if fnmatch.fnmatch(proc.info["name"] or "", name_pattern) or \
                   fnmatch.fnmatch((proc.info["name"] or "").lower(), name_pattern.lower()):
                    pids.append(proc.info["pid"])
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                continue
        return pids

    def watchlist_status(self) -> list[dict]:
        """Return suspend/run status for all configured watchlist processes."""
        cfg = config.get()
        result = []
        for pattern in cfg.watchlist:
            for proc in psutil.process_iter(["pid", "name", "status"]):
                try:
                    name = proc.info["name"] or ""
                    if fnmatch.fnmatch(name, pattern) or fnmatch.fnmatch(name.lower(), pattern.lower()):
                        result.append({
                            "name": name,
                            "pid": proc.info["pid"],
                            "status": proc.info["status"],
                        })
                except (psutil.NoSuchProcess, psutil.AccessDenied):
                    continue
        return result
