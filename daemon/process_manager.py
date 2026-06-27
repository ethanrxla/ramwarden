import fnmatch
import logging
import signal

import psutil

from . import config

log = logging.getLogger("ramwarden.process_manager")


class ProcessManager:
    def suspend(self, name_pattern: str) -> list[int]:
        """Send SIGSTOP to all processes matching name_pattern. Skips already-stopped. Returns suspended PIDs."""
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
