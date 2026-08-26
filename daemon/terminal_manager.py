"""
Detects idle terminal shells and terminal emulator windows.

A shell is "idle" if it has no child processes (nothing running in it).
A shell is "busy" if it has children — agents, servers, etc. — and should
NEVER be auto-closed; the user must close those manually.
"""
import logging
import os
import signal
from dataclasses import dataclass, field

import psutil

log = logging.getLogger("ramwarden.terminals")

_SHELLS = {"bash", "zsh", "fish", "sh", "dash", "ksh"}
_EMULATORS = {
    "gnome-terminal", "gnome-terminal-server", "tilix", "xterm",
    "alacritty", "kitty", "konsole", "xfce4-terminal", "lxterminal",
    "mate-terminal",
}
# These child process names indicate something meaningful is running
_AGENT_NAMES = {
    "python", "python3", "node", "npm", "npx", "cargo", "go",
    "vim", "nvim", "emacs", "htop", "top", "claude", "ollama",
    "ssh", "nc", "ncat", "nmap", "burpsuite",
}


@dataclass
class TerminalInfo:
    pid: int
    shell_name: str
    is_idle: bool
    child_processes: list[str] = field(default_factory=list)
    rss_mb: float = 0.0
    ppid: int = 0


def list_terminals() -> list[TerminalInfo]:
    """Return info about every interactive shell process on the system."""
    results: list[TerminalInfo] = []

    for proc in psutil.process_iter(["pid", "name", "ppid", "memory_info", "status"]):
        try:
            name = proc.info["name"] or ""
            if name.lower() not in _SHELLS:
                continue

            # Must have a controlling terminal (interactive shell)
            try:
                if proc.terminal() is None:
                    continue
            except (psutil.AccessDenied, AttributeError):
                pass

            # Must be a session leader (SID == PID).
            # Docker/script subshells inherit the parent's terminal but share
            # the parent's session — they are NOT session leaders and must be skipped.
            try:
                if os.getsid(proc.info["pid"]) != proc.info["pid"]:
                    continue
            except OSError:
                continue

            children = proc.children(recursive=True)
            child_names = []
            for c in children:
                try:
                    child_names.append(c.name())
                except (psutil.NoSuchProcess, psutil.AccessDenied):
                    pass

            rss = 0.0
            try:
                rss = (proc.info["memory_info"].rss or 0) / (1024 * 1024)
            except Exception:
                pass

            # Idle = no children at all
            is_idle = len(child_names) == 0

            results.append(TerminalInfo(
                pid=proc.info["pid"],
                shell_name=name,
                is_idle=is_idle,
                child_processes=child_names,
                rss_mb=round(rss, 1),
                ppid=proc.info["ppid"] or 0,
            ))
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            continue

    return results


def idle_terminal_summary(terminals: list[TerminalInfo]) -> str:
    idle = [t for t in terminals if t.is_idle]
    busy = [t for t in terminals if not t.is_idle]
    parts = []
    if idle:
        parts.append(f"{len(idle)} idle terminal(s) (PIDs: {', '.join(str(t.pid) for t in idle)})")
    if busy:
        parts.append(
            f"{len(busy)} busy terminal(s) with active processes — do NOT auto-close"
        )
    return "; ".join(parts) if parts else "no interactive terminals found"


def close_idle_terminals(terminals: list[TerminalInfo], timeout: float = 2.0) -> list[int]:
    """
    Close idle shells and return the PIDs that actually exited.

    An interactive bash ignores SIGTERM — it is a session leader with a controlling
    terminal, so the signal is discarded and the shell carries on. The old version
    sent SIGTERM, appended the PID, and reported success, which meant RamWarden
    claimed to close the same six shells on every run while all six stayed alive.

    SIGHUP is what a closing terminal actually sends and what a shell acts on. We
    send that, wait for the process to go, and escalate to SIGKILL only if it does
    not. Nothing is reported closed until the process is confirmed gone.
    """
    targets = []
    for t in terminals:
        if not t.is_idle:
            continue
        try:
            proc = psutil.Process(t.pid)
            proc.send_signal(signal.SIGHUP)
            targets.append((t, proc))
        except (psutil.NoSuchProcess, psutil.AccessDenied) as e:
            log.warning("Could not signal PID %d: %s", t.pid, e)

    if not targets:
        return []

    gone, alive = psutil.wait_procs([p for _, p in targets], timeout=timeout)

    for proc in alive:
        try:
            proc.kill()
            log.info("PID %d ignored SIGHUP — escalating to SIGKILL", proc.pid)
        except (psutil.NoSuchProcess, psutil.AccessDenied) as e:
            log.warning("Could not kill PID %d: %s", proc.pid, e)
    if alive:
        killed, still_alive = psutil.wait_procs(alive, timeout=timeout)
        gone.extend(killed)
        for proc in still_alive:
            log.warning("PID %d survived SIGKILL — not reporting it closed", proc.pid)

    closed = {p.pid for p in gone}
    for t, _ in targets:
        if t.pid in closed:
            log.info("Closed idle terminal shell PID %d (%s)", t.pid, t.shell_name)
    return [t.pid for t, _ in targets if t.pid in closed]
