"""
TDD for terminal_manager — ensure only session-leader shells are closed,
not Docker/script subshells that inherit a terminal from their parent.
"""
import os
import signal
import unittest.mock as mock
from unittest.mock import MagicMock, patch
import pytest

from daemon.terminal_manager import list_terminals, close_idle_terminals, TerminalInfo


def _make_proc(pid, name, ppid, tty, sid, children=None):
    """Build a mock psutil.Process for testing."""
    proc = MagicMock()
    proc.info = {
        "pid": pid,
        "name": name,
        "ppid": ppid,
        "memory_info": MagicMock(rss=10 * 1024 * 1024),
        "status": "sleeping",
    }
    proc.terminal.return_value = tty
    proc.children.return_value = [_make_child(c) for c in (children or [])]
    proc.pid = pid
    return proc


def _make_child(name):
    c = MagicMock()
    c.name.return_value = name
    return c


# ── 1. Session leader filter ──────────────────────────────────────────────────

def test_session_leader_shells_included():
    """Shells where SID == PID (opened by terminal emulator) must be listed."""
    procs = [
        _make_proc(pid=1000, name="bash", ppid=500, tty="/dev/pts/1", sid=1000),
    ]
    with patch("psutil.process_iter", return_value=procs), \
         patch("os.getsid", return_value=1000):
        result = list_terminals()
    assert any(t.pid == 1000 for t in result), "Session-leader bash must be included"


def test_subshell_not_included():
    """sh spawned inside a bash session (SID != PID) must NOT be listed."""
    procs = [
        _make_proc(pid=9999, name="sh", ppid=1000, tty="/dev/pts/1", sid=1000),
    ]
    with patch("psutil.process_iter", return_value=procs), \
         patch("os.getsid", side_effect=lambda pid: 1000):  # SID=1000, PID=9999
        result = list_terminals()
    assert not any(t.pid == 9999 for t in result), (
        "sh subshell (SID != PID) must be excluded — it's a Docker/script child"
    )


def test_docker_sh_subshell_not_closed():
    """
    Regression: Docker operations spawn idle `sh` subshells that inherit the
    parent bash's terminal. These must NOT be auto-closed.

    Scenario from prod: antigravity → bash (SID=1112824) → sg → sh (SID=1112824)
    The sh has tty but SID != PID → not a user session.
    """
    procs = [
        _make_proc(pid=1112824, name="bash", ppid=500, tty="/dev/pts/12", sid=1112824),
        _make_proc(pid=1113046, name="sh",   ppid=1112824, tty="/dev/pts/12", sid=1112824),
    ]

    def mock_getsid(pid):
        return {1112824: 1112824, 1113046: 1112824}[pid]

    with patch("psutil.process_iter", return_value=procs), \
         patch("os.getsid", side_effect=mock_getsid):
        result = list_terminals()

    pids = [t.pid for t in result]
    assert 1112824 in pids,  "Parent bash (session leader) should be listed"
    assert 1113046 not in pids, "Docker sh subshell must NOT be listed"


# ── 2. Idle vs busy classification ───────────────────────────────────────────

def test_idle_shell_has_no_children():
    procs = [
        _make_proc(pid=2000, name="bash", ppid=500, tty="/dev/pts/2", sid=2000, children=[]),
    ]
    with patch("psutil.process_iter", return_value=procs), \
         patch("os.getsid", return_value=2000):
        result = list_terminals()
    assert result[0].is_idle is True


def test_busy_shell_has_children():
    procs = [
        _make_proc(pid=2001, name="bash", ppid=500, tty="/dev/pts/3", sid=2001, children=["python3"]),
    ]
    with patch("psutil.process_iter", return_value=procs), \
         patch("os.getsid", return_value=2001):
        result = list_terminals()
    assert result[0].is_idle is False


def _signal_capture():
    """Patch psutil.Process so we can see which signals reach which PID."""
    signalled: dict[int, list] = {}

    def make_mock(pid):
        m = MagicMock()
        m.pid = pid
        m.send_signal.side_effect = lambda sig: signalled.setdefault(pid, []).append(sig)
        m.kill.side_effect = lambda: signalled.setdefault(pid, []).append("KILL")
        return m

    return signalled, make_mock


def test_close_idle_terminals_skips_busy():
    """Only idle shells are signalled; a shell with children is never touched."""
    idle = TerminalInfo(pid=3000, shell_name="bash", is_idle=True)
    busy = TerminalInfo(pid=3001, shell_name="bash", is_idle=False, child_processes=["vim"])

    signalled, make_mock = _signal_capture()
    with patch("psutil.Process", side_effect=make_mock), \
         patch("psutil.wait_procs", side_effect=lambda procs, timeout=None: (list(procs), [])):
        closed = close_idle_terminals([idle, busy])

    assert signal.SIGHUP in signalled.get(3000, []), "Idle shell must be sent SIGHUP"
    assert 3001 not in signalled, "Busy shell must NOT be signalled"
    assert closed == [3000]


def test_interactive_shell_ignoring_sighup_is_escalated():
    """
    An interactive bash discards SIGTERM and may ignore SIGHUP. If it survives, it
    must be killed — and only reported closed once it is actually gone.
    """
    idle = TerminalInfo(pid=3100, shell_name="bash", is_idle=True)
    signalled, make_mock = _signal_capture()

    calls = []

    def fake_wait(procs, timeout=None):
        calls.append(list(procs))
        if len(calls) == 1:
            return [], list(procs)      # survived SIGHUP
        return list(procs), []          # died after SIGKILL

    with patch("psutil.Process", side_effect=make_mock), \
         patch("psutil.wait_procs", side_effect=fake_wait):
        closed = close_idle_terminals([idle])

    assert signal.SIGHUP in signalled[3100]
    assert "KILL" in signalled[3100], "A surviving shell must be escalated"
    assert closed == [3100]


def test_shell_that_survives_is_not_reported_closed():
    """The bug this guards: reporting a still-running shell as closed."""
    idle = TerminalInfo(pid=3200, shell_name="bash", is_idle=True)
    _, make_mock = _signal_capture()

    with patch("psutil.Process", side_effect=make_mock), \
         patch("psutil.wait_procs", side_effect=lambda procs, timeout=None: ([], list(procs))):
        closed = close_idle_terminals([idle])

    assert closed == [], "A shell that never exited must not be counted as closed"


# ── 3. No terminal → excluded ─────────────────────────────────────────────────

def test_shell_without_tty_excluded():
    """Non-interactive shells (no controlling terminal) must be skipped."""
    procs = [
        _make_proc(pid=4000, name="bash", ppid=1, tty=None, sid=4000),
    ]
    with patch("psutil.process_iter", return_value=procs), \
         patch("os.getsid", return_value=4000):
        result = list_terminals()
    assert not any(t.pid == 4000 for t in result)
