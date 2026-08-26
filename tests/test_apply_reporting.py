"""
The window fires work at the daemon and cannot see the event loop, so the only way
a user learns an action finished is the status line. The bug this guards against:
_apply ran fine but nobody reported back, so a completed job sat on screen reading
"freeing 30 item(s)…" forever — and an exception looked identical.
"""
import asyncio

import pytest

from daemon import main


@pytest.fixture
def captured_status(monkeypatch):
    """Stand in for the GTK window and record what it was told."""
    messages: list[str] = []

    class FakeWindow:
        @staticmethod
        def set_status(text):
            messages.append(text)

        @staticmethod
        def is_running():
            return True

        @staticmethod
        def present(*_a, **_kw):
            pass

    monkeypatch.setattr(main, "window", FakeWindow)
    return messages


def _empty_ctx():
    return {"tabs": [], "terminals": [], "private_windows": []}


class _Rec:
    estimated_ram_freed_mb = 0.0


def _sel(**over):
    sel = {"tabs": [], "processes": [], "terminals": [],
           "private": [], "workspaces": False, "goal": ""}
    sel.update(over)
    return sel


async def test_apply_reports_nothing_when_nothing_happened():
    assert await main._apply(_sel(), _empty_ctx(), _Rec()) == {}


async def test_refused_suspend_is_counted_not_swallowed():
    """A gate refusal must show up in the tally rather than looking like success."""
    done = await main._apply(_sel(processes=["qemu-system-x86_64"]), _empty_ctx(), _Rec())
    assert done == {"refused": 1}


async def test_closed_shells_are_tallied(monkeypatch):
    class FakeTerm:
        pid, is_idle, shell_name = 4242, True, "bash"

    monkeypatch.setattr(main, "close_idle_terminals", lambda targets: [t.pid for t in targets])
    ctx = {"tabs": [], "terminals": [FakeTerm()], "private_windows": []}
    done = await main._apply(_sel(terminals=[4242]), ctx, _Rec())
    assert done == {"shells": 1}


async def test_window_is_told_when_the_work_finishes(captured_status):
    main._loop = asyncio.get_running_loop()
    main._last_context = _empty_ctx()
    main._window_apply(_sel())
    await asyncio.sleep(0.3)
    assert captured_status == ["nothing was freed"]


async def test_window_is_told_when_the_work_fails(captured_status, monkeypatch):
    async def boom(*_a, **_kw):
        raise RuntimeError("extension went away")

    monkeypatch.setattr(main, "_apply", boom)
    main._loop = asyncio.get_running_loop()
    main._last_context = _empty_ctx()
    main._window_apply(_sel())
    await asyncio.sleep(0.3)
    assert captured_status and captured_status[0].startswith("failed:")
    assert "extension went away" in captured_status[0]
