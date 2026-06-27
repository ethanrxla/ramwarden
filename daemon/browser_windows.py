"""
OS-level detection of private/incognito/Tor browser contexts.

Two detection paths:
  1. wmctrl — Firefox private windows (Firefox IS visible via XWayland)
  2. psutil  — Brave Tor mode (Brave is a Flatpak, invisible to wmctrl)

Brave incognito (non-Tor) cannot be detected at the OS level — Brave's renderers
carry no --incognito flag and the Flatpak sandbox hides its windows from wmctrl.
The only way to see those tabs is to enable "Allow in Private Windows" in
brave://extensions → RamWarden → Details.
"""
import logging
import os
import subprocess
from dataclasses import dataclass, field

import psutil

log = logging.getLogger("ramwarden.browser_windows")

_DISPLAY = os.environ.get("DISPLAY", ":1")


@dataclass
class PrivateWindow:
    type: str           # "firefox_private" | "brave_tor" | "tor_browser"
    xid: str            # wmctrl xid for close (empty if not accessible via wmctrl)
    pid: int
    title: str
    rss_mb: float
    can_get_urls: bool = False   # True only if extension has incognito permission
    closeable: bool = True       # False for Brave Flatpak (no wmctrl access)


# ── Helpers ───────────────────────────────────────────────────────────────────

def _wmctrl(*args) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["wmctrl"] + list(args),
        env={**os.environ, "DISPLAY": _DISPLAY},
        capture_output=True, text=True, timeout=5,
    )


def _get_rss_tree(pid: int) -> float:
    """Sum RSS of a process and all its children (MB)."""
    try:
        proc = psutil.Process(pid)
        total = proc.memory_info().rss
        for child in proc.children(recursive=True):
            try:
                total += child.memory_info().rss
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                pass
        return total / (1024 * 1024)
    except (psutil.NoSuchProcess, psutil.AccessDenied):
        return 0.0


def _brave_main_rss() -> float:
    """Total RSS of all Brave processes."""
    total = 0.0
    for proc in psutil.process_iter(["name", "memory_info"]):
        try:
            if (proc.info["name"] or "").lower() in ("brave", "brave-browser"):
                total += (proc.info["memory_info"].rss or 0)
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            pass
    return total / (1024 * 1024)


# ── psutil-based detection (Flatpak Brave) ────────────────────────────────────

def _detect_brave_tor() -> PrivateWindow | None:
    """
    Brave has a built-in Tor mode. When active it spawns a tor.mojom.TorLauncher
    utility process. We can detect this even inside the Flatpak sandbox.
    We can't close it via wmctrl, but we report its RAM.
    """
    for proc in psutil.process_iter(["pid", "name", "cmdline"]):
        try:
            name = (proc.info["name"] or "").lower()
            cmd  = proc.info["cmdline"] or []
            if "brave" in name and any("tor.mojom.TorLauncher" in a for a in cmd):
                # Estimate Tor-related RAM (the launcher + the actual tor daemon)
                rss = _get_rss_tree(proc.info["pid"])
                # Add the standalone tor daemon if it's running from Brave's config dir
                for tp in psutil.process_iter(["pid", "name", "exe"]):
                    try:
                        exe = tp.info.get("exe") or ""
                        if "brave" in exe.lower() and "tor" in (tp.info["name"] or "").lower():
                            rss += _get_rss_tree(tp.info["pid"])
                    except (psutil.NoSuchProcess, psutil.AccessDenied):
                        pass
                return PrivateWindow(
                    type="brave_tor",
                    xid="",
                    pid=proc.info["pid"],
                    title="Brave (Private Window with Tor)",
                    rss_mb=round(rss, 1),
                    can_get_urls=False,   # can't reach Tor tabs via extension
                    closeable=False,      # Flatpak Brave not reachable via wmctrl
                )
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            continue
    return None


def _detect_standalone_tor() -> PrivateWindow | None:
    """Detect a standalone Tor Browser (not Brave's built-in Tor)."""
    for proc in psutil.process_iter(["pid", "name", "exe"]):
        try:
            exe = proc.info.get("exe") or ""
            name = (proc.info["name"] or "").lower()
            if name in ("firefox", "firefox-bin") and (
                "torbrowser" in exe.lower() or "tor-browser" in exe.lower()
            ):
                return PrivateWindow(
                    type="tor_browser",
                    xid="",
                    pid=proc.info["pid"],
                    title="Tor Browser",
                    rss_mb=_get_rss_tree(proc.info["pid"]),
                    can_get_urls=False,
                    closeable=False,
                )
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            continue
    return None


# ── wmctrl-based detection (Firefox — visible via XWayland) ──────────────────

def _detect_firefox_private() -> list[PrivateWindow]:
    """
    Firefox private windows show '(Private Browsing)' in their window title.
    Firefox is NOT a Flatpak here so it appears in wmctrl.
    """
    try:
        result = _wmctrl("-l", "-p")
        if result.returncode != 0:
            return []
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return []

    found = []
    for line in result.stdout.splitlines():
        parts = line.split(None, 4)
        if len(parts) < 5:
            continue
        xid, _ws, pid_str, _host, title = parts
        title_lower = title.lower()
        if "private browsing" in title_lower or "navigation privée" in title_lower:
            try:
                pid = int(pid_str)
            except ValueError:
                continue
            found.append(PrivateWindow(
                type="firefox_private",
                xid=xid,
                pid=pid,
                title=title,
                rss_mb=_get_rss_tree(pid),
                can_get_urls=False,  # updated by caller if extension has permission
                closeable=True,
            ))
    return found


# ── Public API ────────────────────────────────────────────────────────────────

def detect_private_windows(extension_has_incognito: bool = False) -> list[PrivateWindow]:
    """
    Find all private/incognito/Tor browser contexts visible at the OS level.

    What we can detect:
      - Firefox Private Browsing windows (via wmctrl title)
      - Brave built-in Tor mode (via psutil TorLauncher process)
      - Standalone Tor Browser (via psutil exe path)

    What we CANNOT detect:
      - Brave incognito (Flatpak sandbox, no wmctrl visibility, no process flag)
        → user must enable "Allow in Private Windows" in brave://extensions
    """
    found: list[PrivateWindow] = []

    # Firefox private windows (wmctrl)
    for pw in _detect_firefox_private():
        pw.can_get_urls = extension_has_incognito
        found.append(pw)

    # Brave built-in Tor (psutil)
    brave_tor = _detect_brave_tor()
    if brave_tor:
        found.append(brave_tor)

    # Standalone Tor Browser (psutil)
    standalone_tor = _detect_standalone_tor()
    if standalone_tor:
        found.append(standalone_tor)

    if found:
        log.info(
            "Private contexts: %s",
            ", ".join(f"{w.type}({w.rss_mb:.0f}MB)" for w in found)
        )
    return found


def close_private_window(xid: str) -> bool:
    """Send WM_DELETE_WINDOW to a private window. Only works for wmctrl-visible windows."""
    if not xid:
        return False
    try:
        r = _wmctrl("-ic", xid)
        return r.returncode == 0
    except Exception as e:
        log.warning("close_private_window(%s) failed: %s", xid, e)
        return False
