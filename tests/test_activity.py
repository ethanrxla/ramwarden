"""
TDD for the activity gate — the layer that decides what RamWarden is allowed to
touch. The bug this exists to prevent: before dynamic detection, any process the
name lists didn't recognise fell through to "idle", which made a 4 GB virtual
machine a suspend candidate.

These tests build signals directly rather than sampling the real system, so they
assert on policy, not on whatever happens to be running.
"""
import psutil
import pytest

from daemon.activity import (
    ActivityDetector,
    ActivitySignals,
    CPU_BUSY_SECONDS,
    classify_role,
)

WATCHLIST = ["Discord", "burpsuite"]


def _detector(*signals: ActivitySignals, warm: bool = True) -> ActivityDetector:
    """A detector pre-loaded with hand-built signals and scored."""
    det = ActivityDetector()
    det._ticks = 2 if warm else 1
    det._signals = {s.pid: s for s in signals}
    for s in det._signals.values():
        det._score(s)
    return det


def _sig(pid=1, name="thing", rss_mb=500.0, **kw) -> ActivitySignals:
    return ActivitySignals(pid=pid, name=name, rss_mb=rss_mb,
                           role=kw.pop("role", classify_role(name)), **kw)


# ── 1. Structural protection ─────────────────────────────────────────────────

@pytest.mark.parametrize("name,expected", [
    ("qemu-system-x86_64", "HYPERVISOR"),
    ("VirtualBoxVM", "HYPERVISOR"),
    ("containerd-shim-runc-v2", "CONTAINER"),
    ("dockerd", "CONTAINER"),
    ("cosmic-comp", "COMPOSITOR"),
    ("gnome-shell", "COMPOSITOR"),
    ("claude", "AGENT"),
    ("cosmic-term", "TERMINAL"),
    ("ffmpeg", "MEDIA"),
    ("cargo", "BUILD"),
    ("syncthing", "SYNC"),
])
def test_structural_roles_are_recognised(name, expected):
    assert classify_role(name) == expected


def test_virtual_machine_is_never_suspendable_even_if_watchlisted():
    """The regression this whole module exists for: a quiet VM is not an idle app."""
    det = _detector(_sig(pid=10, name="qemu-system-x86_64", rss_mb=4200.0))
    assert det.get(10).verdict == "PROTECTED"
    assert det.get(10).protection == "structural"

    allowed, why = det.may_suspend("qemu-system-x86_64", ["qemu-system-x86_64"])
    assert allowed is False
    assert "virtual machine" in why


def test_structural_protection_cannot_be_forced():
    from daemon.process_manager import ProcessManager

    det = _detector(_sig(pid=11, name="dockerd"))
    import daemon.activity as activity
    original, activity._detector = activity._detector, det
    try:
        allowed, why = ProcessManager._gate("dockerd", force=True)
    finally:
        activity._detector = original
    assert allowed is False
    assert "not forceable" in why


# ── 2. Servers ───────────────────────────────────────────────────────────────

def test_listening_socket_protects_an_unknown_process():
    """A dev server nobody wrote down is still a server."""
    det = _detector(_sig(pid=20, name="my-api", listening_ports=[8000]))
    sig = det.get(20)
    assert sig.verdict == "PROTECTED"
    assert sig.protection == "serving"
    assert "8000" in sig.reasons[0]

    allowed, why = det.may_suspend("my-api", WATCHLIST)
    assert allowed is False
    assert "not on the watchlist" in why


def test_watchlist_overrides_soft_serving_protection_with_a_warning():
    """Discord's RPC port is not a service anyone depends on — the user decides."""
    det = _detector(_sig(pid=21, name="Discord", listening_ports=[6463]))
    allowed, why = det.may_suspend("Discord", WATCHLIST)
    assert allowed is True
    assert "6463" in why   # the consequence stays visible


# ── 3. In-use signals ────────────────────────────────────────────────────────

@pytest.mark.parametrize("kwargs,fragment", [
    ({"is_focused": True}, "looking at"),
    ({"playing_audio": True}, "playing audio"),
    ({"cpu_seconds_recent": CPU_BUSY_SECONDS + 0.1}, "CPU"),
    ({"age_minutes": 1.0}, "started"),
    ({"active_descendant": "cargo (pid 99)"}, "child still working"),
])
def test_each_signal_marks_a_process_in_use(kwargs, fragment):
    fields = {"age_minutes": 600.0, **kwargs}
    det = _detector(_sig(pid=30, name="Discord", **fields))
    sig = det.get(30)
    assert sig.verdict == "IN_USE"
    assert any(fragment in r for r in sig.reasons)
    assert det.may_suspend("Discord", WATCHLIST)[0] is False


def test_quiet_watchlisted_app_is_reclaimable():
    det = _detector(_sig(pid=31, name="Discord", rss_mb=500.0, age_minutes=600.0))
    assert det.get(31).verdict == "IDLE"
    allowed, _ = det.may_suspend("Discord", WATCHLIST)
    assert allowed is True
    assert [s.pid for s in det.reclaimable(WATCHLIST)] == [31]


# ── 4. Fail-closed behaviour ─────────────────────────────────────────────────

def test_nothing_is_idle_before_two_samples():
    """One sample gives no CPU delta, so 'no CPU' means 'not measured yet'."""
    det = _detector(_sig(pid=40, name="Discord", age_minutes=600.0), warm=False)
    assert det.warm is False
    assert det.get(40).verdict == "IN_USE"
    assert det.may_suspend("Discord", WATCHLIST)[0] is False


def test_reclaimable_never_leaves_the_watchlist():
    det = _detector(
        _sig(pid=50, name="Discord", age_minutes=600.0),
        _sig(pid=51, name="steamwebhelper", rss_mb=900.0, age_minutes=600.0),
    )
    assert det.get(51).verdict == "IDLE"          # idle, but off the list
    names = {s.name for s in det.reclaimable(WATCHLIST)}
    assert names == {"Discord"}


def test_stopped_processes_are_not_offered_again():
    det = _detector(_sig(pid=60, name="Discord", age_minutes=600.0,
                         status=psutil.STATUS_STOPPED))
    assert det.reclaimable(WATCHLIST) == []


def test_small_processes_are_not_worth_the_risk():
    det = _detector(_sig(pid=61, name="Discord", rss_mb=20.0, age_minutes=600.0))
    assert det.reclaimable(WATCHLIST) == []


# ── 5. Group semantics ───────────────────────────────────────────────────────

def test_the_busiest_member_decides_the_whole_group():
    """Suspending half an app is worse than suspending none of it."""
    det = _detector(
        _sig(pid=70, name="Discord", age_minutes=600.0),
        _sig(pid=71, name="Discord", age_minutes=600.0, playing_audio=True),
    )
    verdict, reasons, pids = det.verdict_for_name("Discord")
    assert verdict == "IN_USE"
    assert set(pids) == {70, 71}
    assert det.may_suspend("Discord", WATCHLIST)[0] is False


def test_activity_propagates_up_the_process_tree():
    """A shell whose child is compiling is itself in use."""
    det = ActivityDetector()
    det._ticks = 2
    parent = _sig(pid=80, name="bash", rss_mb=200.0, age_minutes=600.0)
    child = _sig(pid=81, name="cargo", rss_mb=200.0, age_minutes=600.0)
    det._signals = {80: parent, 81: child}
    det._propagate(det._signals, {81: 80, 80: 1})
    for s in det._signals.values():
        det._score(s)
    assert "cargo" in parent.active_descendant
    assert parent.verdict == "IN_USE"


# ── 6. Suspended registry and auto-resume ────────────────────────────────────

def test_registry_only_tracks_what_ramwarden_froze(monkeypatch):
    """
    A process the user stopped themselves (Ctrl-Z, a debugger) must never be swept
    up by auto-resume — membership in the registry, not the STOPPED status, is what
    makes something ours to wake.
    """
    import daemon.process_manager as pmod

    monkeypatch.setattr(pmod, "_suspended", {}, raising=False)
    assert pmod.suspended_entries() == []

    entry = pmod.SuspendedEntry(name="Discord", pids=[111, 222])
    pmod._suspended["Discord"] = entry

    # Neither PID is actually stopped, so the entry prunes itself away.
    monkeypatch.setattr(pmod.psutil, "Process", _raise_no_such)
    assert pmod.suspended_entries() == []


def _raise_no_such(pid):
    raise psutil.NoSuchProcess(pid)


def test_monitor_auto_resumes_only_below_the_threshold(monkeypatch):
    """Pressure gone → give the frozen app back without being asked."""
    from daemon import monitor as mon_mod
    import daemon.process_manager as pmod

    resumed: list[str] = []

    class FakePM:
        def resume_entry(self, entry):
            resumed.append(entry.name)
            return list(entry.pids)

    entry = pmod.SuspendedEntry(name="Discord", pids=[1])
    monkeypatch.setattr(pmod, "suspended_entries", lambda: [entry])
    monkeypatch.setattr(pmod, "ProcessManager", FakePM)

    m = mon_mod.Monitor(on_threshold=lambda s: None)

    snap = mon_mod.RAMSnapshot(total_mb=1000, used_mb=400, percent=40.0, processes=[])
    monkeypatch.setattr(mon_mod, "snapshot", lambda: snap)
    monkeypatch.setattr(mon_mod, "tick", lambda: None, raising=False)
    monkeypatch.setattr("daemon.process_profiler.tick", lambda: None)
    monkeypatch.setattr("daemon.activity.tick", lambda: None)

    m._tick()
    assert resumed == ["Discord"], "below the threshold, frozen apps must be woken"

    # Under pressure it must NOT resume — that would undo the relief immediately.
    resumed.clear()
    busy = mon_mod.RAMSnapshot(total_mb=1000, used_mb=900, percent=90.0, processes=[])
    monkeypatch.setattr(mon_mod, "snapshot", lambda: busy)
    m._tick()
    assert resumed == []
