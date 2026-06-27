"""
Workspace management for RamWarden.

Works for XWayland apps (Firefox, gnome-terminal, alacritty, etc.) via wmctrl.
cosmic-term runs natively on COSMIC Wayland — it does NOT appear in wmctrl.
COSMIC's compositor (cosmic-comp) has no public D-Bus workspace API as of 2026.

The auto_sort() function moves any XWayland windows that violate rules.
The workspace_watcher() thread runs continuously, auto-sorting every 15s.
"""
import logging
import os
import subprocess
import threading
import time
from dataclasses import dataclass, field

import psutil

log = logging.getLogger("ramwarden.workspaces")

_DISPLAY = os.environ.get("DISPLAY", ":1")

# Names of terminal emulators visible to XWayland (wmctrl can see these)
_XWAYLAND_TERMINALS = {
    "gnome-terminal", "gnome-terminal-server", "tilix", "alacritty",
    "kitty", "konsole", "xfce4-terminal", "lxterminal",
    "mate-terminal", "terminator", "xterm",
}

# cosmic-term and Brave are Wayland-native Flatpaks — wmctrl can NOT see them
_WAYLAND_NATIVE = {"cosmic-term", "brave", "brave-browser"}


def _wmctrl(*args) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["wmctrl"] + list(args),
        env={**os.environ, "DISPLAY": _DISPLAY},
        capture_output=True, text=True, timeout=5,
    )


def _wmctrl_available() -> bool:
    try:
        r = subprocess.run(
            ["wmctrl", "-h"],
            env={**os.environ, "DISPLAY": _DISPLAY},
            capture_output=True, timeout=3,
        )
        return r.returncode in (0, 1)
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return False


@dataclass
class WindowInfo:
    xid: str
    workspace: int   # -1 = sticky
    pid: int
    name: str
    wm_class: str = ""       # instance name (e.g. "Navigator")
    wm_class_app: str = ""   # app class name (e.g. "firefox")


@dataclass
class WorkspaceLayout:
    n_workspaces: int
    active_workspace: int
    windows: list[WindowInfo] = field(default_factory=list)
    wayland_native_terminals: list[str] = field(default_factory=list)


def _get_wm_class(xid: str) -> tuple[str, str]:
    """Return (instance_name, app_class) both lowercased. E.g. ("navigator", "firefox")."""
    try:
        r = subprocess.run(
            ["xprop", "-id", xid, "WM_CLASS"],
            env={**os.environ, "DISPLAY": _DISPLAY},
            capture_output=True, text=True, timeout=2,
        )
        if r.returncode == 0:
            parts = r.stdout.split('"')
            instance = parts[1].lower() if len(parts) >= 2 else ""
            app_cls  = parts[3].lower() if len(parts) >= 4 else instance
            return instance, app_cls
    except Exception:
        pass
    return "", ""


def get_layout() -> WorkspaceLayout | None:
    if not _wmctrl_available():
        log.warning("wmctrl not found — install with: sudo apt install wmctrl")
        return None
    try:
        l_result = _wmctrl("-l", "-p")
        if l_result.returncode != 0 or not l_result.stdout.strip():
            return None

        # COSMIC DE does NOT expose _NET_NUMBER_OF_DESKTOPS to XWayland,
        # so `wmctrl -d` always fails. We parse the window list directly and
        # derive workspace count from the max workspace index seen.
        d_result = _wmctrl("-d")
        n_ws, active_ws = 1, 0
        if d_result.returncode == 0:
            for line in d_result.stdout.splitlines():
                parts = line.split()
                if len(parts) < 2:
                    continue
                n_ws += 1
                if parts[1] == "*":
                    try:
                        active_ws = int(parts[0])
                    except ValueError:
                        pass

        windows: list[WindowInfo] = []
        for line in l_result.stdout.splitlines():
            parts = line.split(None, 4)
            if len(parts) < 5:
                continue
            xid, ws_str, pid_str, _host, name = parts
            try:
                ws = int(ws_str)
                pid = int(pid_str)
            except ValueError:
                continue
            win = WindowInfo(xid=xid, workspace=ws, pid=pid, name=name)
            win.wm_class, win.wm_class_app = _get_wm_class(xid)
            windows.append(win)

        # Derive workspace count from actual window data if wmctrl -d failed
        if windows:
            n_ws = max(n_ws, max(w.workspace for w in windows if w.workspace >= 0) + 1)

        # Detect Wayland-native terminals (not visible in wmctrl)
        wayland_terminals = []
        for proc in psutil.process_iter(["name"]):
            try:
                name = (proc.info["name"] or "").lower()
                if name in _WAYLAND_NATIVE and "term" in name:
                    wayland_terminals.append(name)
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                pass

        return WorkspaceLayout(
            n_workspaces=n_ws,
            active_workspace=active_ws,
            windows=windows,
            wayland_native_terminals=list(set(wayland_terminals)),
        )
    except Exception as e:
        log.warning("get_layout failed: %s", e)
        return None


def _ensure_workspaces(n: int):
    try:
        _wmctrl("-n", str(n))
    except Exception as e:
        log.warning("ensure_workspaces(%d) failed: %s", n, e)


def move_window(xid: str, target_workspace: int) -> bool:
    try:
        _ensure_workspaces(target_workspace + 1)
        r = _wmctrl("-i", "-r", xid, "-t", str(target_workspace))
        return r.returncode == 0
    except Exception as e:
        log.warning("move_window %s → %d failed: %s", xid, target_workspace, e)
        return False


def auto_sort(rules: list[dict]) -> list[dict]:
    """
    Apply workspace rules from config to currently open XWayland windows.
    Returns list of {"window", "xid", "moved_to"} for each window moved.

    Wayland-native apps (cosmic-term, Brave) are silently skipped — they don't
    appear in wmctrl and cannot be moved without a COSMIC compositor API.
    """
    layout = get_layout()
    if layout is None:
        return []

    max_ws = max((r.get("workspace", 0) for r in rules), default=0)
    if max_ws >= layout.n_workspaces:
        _ensure_workspaces(max_ws + 1)

    if layout.wayland_native_terminals:
        log.debug(
            "Wayland-native terminals detected (%s) — cannot move via wmctrl",
            ", ".join(layout.wayland_native_terminals),
        )

    moved = []
    for win in layout.windows:
        for rule in rules:
            match_str  = rule.get("match", "").lower()
            target     = rule.get("workspace", 0)
            match_type = rule.get("match_type", "class")

            if match_type == "class":
                # Match against both instance name and app class (e.g. "Navigator" + "firefox")
                haystack = win.wm_class + " " + win.wm_class_app
            else:
                haystack = win.name.lower()

            if match_str and match_str in haystack and win.workspace != target:
                if move_window(win.xid, target):
                    moved.append({"window": win.name, "xid": win.xid, "moved_to": target})
                    log.info("Moved '%s' to workspace %d", win.name, target)
                break

    return moved


# ── Background workspace watcher ──────────────────────────────────────────────

_watcher_thread: threading.Thread | None = None
_watcher_stop   = threading.Event()


def start_workspace_watcher(rules: list[dict], interval_s: float = 15.0):
    """
    Background thread: checks every `interval_s` seconds whether any XWayland
    windows are on the wrong workspace and moves them.

    Silently skips Wayland-native apps (cosmic-term, Brave Flatpak).
    """
    global _watcher_thread, _watcher_stop

    if not rules:
        return
    if not _wmctrl_available():
        log.info("wmctrl not available — workspace watcher disabled")
        return

    _watcher_stop.clear()

    def _run():
        log.info("Workspace watcher started (interval=%.0fs, rules=%d)", interval_s, len(rules))
        while not _watcher_stop.wait(interval_s):
            try:
                moved = auto_sort(rules)
                if moved:
                    log.info(
                        "Workspace watcher moved %d window(s): %s",
                        len(moved),
                        ", ".join(f"'{m['window']}' → ws{m['moved_to']}" for m in moved),
                    )
            except Exception as e:
                log.debug("Workspace watcher error: %s", e)
        log.info("Workspace watcher stopped")

    _watcher_thread = threading.Thread(target=_run, name="workspace-watcher", daemon=True)
    _watcher_thread.start()


def stop_workspace_watcher():
    _watcher_stop.set()
