"""
The RamWarden window — a small, always-available panel rather than a popup.

The old flow only ever showed itself as a modal dialog at the moment RAM crossed
a threshold, which meant the one time you saw RamWarden was the one time you were
least inclined to read it. This is a persistent ~420x600 window instead: it sits
out of the way showing live memory pressure and what RamWarden believes you are
using, and when a threshold fires it raises itself with a recommendation already
loaded rather than seizing the screen with a modal.

It runs its own GTK main loop on a dedicated thread. Everything that touches the
daemon goes through the WindowController the daemon hands in, so this module owns
no policy — it renders state and reports clicks.

Closing the window hides it; the daemon keeps running and can present it again.
"""
from __future__ import annotations

import logging
import threading
from dataclasses import dataclass, field
from typing import Callable

log = logging.getLogger("ramwarden.window")


@dataclass
class WindowController:
    """
    The daemon's side of the window. Every callback is invoked on the GTK thread,
    so anything touching the event loop must hand off rather than block.
    """
    get_state: Callable[[], dict]
    request_analysis: Callable[[str], None]
    apply_selection: Callable[[dict], None]
    resume_all: Callable[[], None]


# Module state — one window per process.
_app: "_RamWardenWindow | None" = None
_lock = threading.Lock()


def start(controller: WindowController) -> bool:
    """
    Start the window on its own thread. Returns False if GTK is unavailable, in
    which case the daemon falls back to the modal prompt.
    """
    global _app
    with _lock:
        if _app is not None:
            return True
        try:
            import gi
            gi.require_version("Gtk", "3.0")
            from gi.repository import Gtk  # noqa: F401
        except Exception as e:
            log.warning("GTK unavailable (%s) — window disabled", e)
            return False

        _app = _RamWardenWindow(controller)
        t = threading.Thread(target=_app.run, name="ramwarden-window", daemon=True)
        t.start()
        log.info("RamWarden window started")
        return True


def is_running() -> bool:
    return _app is not None


def present(recommendation=None, context: dict | None = None) -> None:
    """Raise the window, optionally loading a recommendation for the user to act on."""
    if _app is not None:
        _app.present(recommendation, context or {})


def set_status(text: str) -> None:
    """
    Report an outcome into the window's status line. Safe to call from any thread —
    the daemon uses this to close the loop on work the window kicked off, so a
    finished job stops reading as an in-progress one.
    """
    if _app is not None:
        _app.set_status_threadsafe(text)


def stop() -> None:
    global _app
    if _app is not None:
        _app.quit()
        _app = None


# ── Formatting helpers ───────────────────────────────────────────────────────

def _fmt_mb(mb: float) -> str:
    return f"{mb / 1024:.1f} GB" if mb >= 1024 else f"{mb:.0f} MB"


def _domain(url: str) -> str:
    from urllib.parse import urlparse
    try:
        return urlparse(url).netloc or url[:36]
    except Exception:
        return url[:36]


def _age(minutes: int) -> str:
    if minutes < 60:
        return f"{minutes}m"
    if minutes < 1440:
        return f"{minutes // 60}h"
    return f"{minutes // 1440}d"


_VERDICT_COLOR = {
    "PROTECTED": "#60a5fa",   # blue — structurally off-limits
    "IN_USE":    "#4ade80",   # green — you are using it
    "IDLE":      "#facc15",   # amber — reclaimable
}


class _RamWardenWindow:
    REFRESH_SECONDS = 4

    def __init__(self, controller: WindowController):
        self.ctl = controller
        self._rec = None
        self._context: dict = {}
        self._busy = False

    # ── lifecycle ────────────────────────────────────────────────────────────

    def run(self):
        import gi
        gi.require_version("Gtk", "3.0")
        from gi.repository import Gtk, GLib, GObject

        self.Gtk, self.GLib, self.GObject = Gtk, GLib, GObject
        self._build()
        GLib.timeout_add_seconds(self.REFRESH_SECONDS, self._refresh)
        self._refresh()
        Gtk.main()

    def quit(self):
        if hasattr(self, "GLib"):
            self.GLib.idle_add(self.Gtk.main_quit)

    def present(self, recommendation, context: dict):
        if not hasattr(self, "GLib"):
            return
        self.GLib.idle_add(self._present_on_ui, recommendation, context)

    def set_status_threadsafe(self, text: str):
        if not hasattr(self, "GLib"):
            return

        def _apply_text():
            self._busy = False
            self.apply_btn.set_sensitive(True)
            self._set_status(text)
            self._refresh()
            return False

        self.GLib.idle_add(_apply_text)

    # ── construction ─────────────────────────────────────────────────────────

    def _build(self):
        Gtk, GObject = self.Gtk, self.GObject

        self.win = Gtk.Window(title="RamWarden")
        self.win.set_default_size(420, 600)
        self.win.set_role("ramwarden")
        # Closing is "get out of my way", not "shut down the daemon".
        self.win.connect("delete-event", self._on_delete)

        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        outer.set_margin_top(12)
        outer.set_margin_bottom(12)
        outer.set_margin_start(14)
        outer.set_margin_end(14)
        self.win.add(outer)

        # ── memory pressure ──────────────────────────────────────────────────
        self.ram_label = Gtk.Label(xalign=0)
        outer.pack_start(self.ram_label, False, False, 0)

        self.ram_bar = Gtk.ProgressBar()
        self.ram_bar.set_show_text(False)
        outer.pack_start(self.ram_bar, False, False, 0)

        self.detail_label = Gtk.Label(xalign=0)
        self.detail_label.get_style_context().add_class("dim-label")
        self.detail_label.set_line_wrap(True)
        outer.pack_start(self.detail_label, False, False, 0)

        outer.pack_start(Gtk.Separator(), False, False, 4)

        # ── what RamWarden thinks you are using ──────────────────────────────
        # columns: name, size, verdict, reason, colour, pid
        self.proc_store = Gtk.ListStore(str, str, str, str, str, int)
        view = Gtk.TreeView(model=self.proc_store)
        view.set_headers_visible(True)
        view.set_tooltip_column(3)

        name_r = Gtk.CellRendererText()
        name_r.set_property("ellipsize", 3)
        view.append_column(Gtk.TreeViewColumn("Process", name_r, text=0))

        size_r = Gtk.CellRendererText()
        size_r.set_property("xalign", 1.0)
        col = Gtk.TreeViewColumn("RAM", size_r, text=1)
        col.set_min_width(70)
        view.append_column(col)

        verdict_r = Gtk.CellRendererText()
        col = Gtk.TreeViewColumn("Status", verdict_r, text=2, foreground=4)
        col.set_min_width(84)
        view.append_column(col)

        scroll = Gtk.ScrolledWindow()
        scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroll.set_min_content_height(190)
        scroll.add(view)
        outer.pack_start(scroll, True, True, 0)

        legend = Gtk.Label(xalign=0)
        legend.set_markup(
            "<small>"
            "<span foreground='#60a5fa'>■</span> protected  "
            "<span foreground='#4ade80'>■</span> in use  "
            "<span foreground='#facc15'>■</span> idle"
            "  ·  hover a row for the reason</small>"
        )
        outer.pack_start(legend, False, False, 0)

        # ── recommendation area (populated when analysis has something) ──────
        self.rec_frame = Gtk.Frame(label="Suggested cleanup")
        rec_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        rec_box.set_margin_top(6)
        rec_box.set_margin_bottom(6)
        rec_box.set_margin_start(8)
        rec_box.set_margin_end(8)
        self.rec_frame.add(rec_box)

        self.rec_summary = Gtk.Label(xalign=0, wrap=True)
        rec_box.pack_start(self.rec_summary, False, False, 0)

        # columns: checked, label, detail, kind, id-as-str
        self.item_store = Gtk.ListStore(
            GObject.TYPE_BOOLEAN, str, str, str, str
        )
        item_view = Gtk.TreeView(model=self.item_store)
        item_view.set_headers_visible(False)

        toggle = Gtk.CellRendererToggle()
        toggle.connect("toggled", self._on_item_toggled)
        item_view.append_column(Gtk.TreeViewColumn("", toggle, active=0))

        lbl_r = Gtk.CellRendererText()
        lbl_r.set_property("ellipsize", 3)
        c = Gtk.TreeViewColumn("Item", lbl_r, text=1)
        c.set_expand(True)
        item_view.append_column(c)

        det_r = Gtk.CellRendererText()
        det_r.set_property("foreground", "gray")
        item_view.append_column(Gtk.TreeViewColumn("", det_r, text=2))

        item_scroll = Gtk.ScrolledWindow()
        item_scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        item_scroll.set_min_content_height(120)
        item_scroll.add(item_view)
        rec_box.pack_start(item_scroll, True, True, 0)

        apply_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        self.apply_btn = Gtk.Button(label="Free selected")
        self.apply_btn.get_style_context().add_class("suggested-action")
        self.apply_btn.connect("clicked", self._on_apply)
        dismiss_btn = Gtk.Button(label="Dismiss")
        dismiss_btn.connect("clicked", self._on_dismiss)
        apply_box.pack_start(self.apply_btn, True, True, 0)
        apply_box.pack_start(dismiss_btn, False, False, 0)
        rec_box.pack_start(apply_box, False, False, 0)

        outer.pack_start(self.rec_frame, False, False, 0)

        # ── goal + actions ───────────────────────────────────────────────────
        goal_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        self.goal_entry = Gtk.Entry()
        self.goal_entry.set_placeholder_text("What are you working on? (optional)")
        self.goal_entry.set_hexpand(True)
        self.goal_entry.connect("activate", self._on_analyze)
        self.analyze_btn = Gtk.Button(label="Analyze")
        self.analyze_btn.connect("clicked", self._on_analyze)
        goal_box.pack_start(self.goal_entry, True, True, 0)
        goal_box.pack_start(self.analyze_btn, False, False, 0)
        outer.pack_start(goal_box, False, False, 0)

        action_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=6)
        resume_btn = Gtk.Button(label="Resume suspended")
        resume_btn.connect("clicked", self._on_resume)
        action_box.pack_start(resume_btn, False, False, 0)
        self.status_label = Gtk.Label(xalign=1)
        self.status_label.get_style_context().add_class("dim-label")
        self.status_label.set_ellipsize(3)
        action_box.pack_end(self.status_label, True, True, 0)
        outer.pack_start(action_box, False, False, 0)

        self.win.show_all()
        self.rec_frame.hide()

    # ── refresh ──────────────────────────────────────────────────────────────

    def _refresh(self) -> bool:
        try:
            state = self.ctl.get_state()
        except Exception as e:
            log.debug("state fetch failed: %s", e)
            return True

        pct = state.get("percent", 0.0)
        used = state.get("used_mb", 0.0)
        total = state.get("total_mb", 0.0)
        warn = state.get("warn_percent", 75.0)

        colour = "#4ade80" if pct < warn else ("#facc15" if pct < warn + 10 else "#f87171")
        self.ram_label.set_markup(
            f"<span size='x-large' weight='bold' foreground='{colour}'>{pct:.0f}%</span>"
            f"  <span foreground='gray'>{_fmt_mb(used)} of {_fmt_mb(total)}</span>"
        )
        self.ram_bar.set_fraction(min(pct / 100.0, 1.0))

        totals = state.get("totals_mb") or {}
        if totals:
            self.detail_label.set_markup(
                f"<small>protected {_fmt_mb(totals.get('PROTECTED', 0))}"
                f"  ·  in use {_fmt_mb(totals.get('IN_USE', 0))}"
                f"  ·  idle {_fmt_mb(totals.get('IDLE', 0))}"
                f"  ·  {state.get('browsers_connected', 0)} browser(s) connected</small>"
            )
        else:
            self.detail_label.set_markup(
                "<small>sampling — the monitor ticks every 10 seconds</small>"
            )

        self._fill_processes(state.get("processes") or [])
        return True

    def _fill_processes(self, procs: list[dict]):
        # Rebuilding the store on every tick would fight the user's scroll position,
        # so update in place while the shape of the list is unchanged.
        rows = [
            (
                p.get("name", "?"),
                _fmt_mb(p.get("rss_mb", 0.0)),
                p.get("verdict", ""),
                "; ".join(p.get("reasons") or []) or "—",
                _VERDICT_COLOR.get(p.get("verdict", ""), "gray"),
                int(p.get("pid", 0)),
            )
            for p in procs
        ]
        if len(rows) == len(self.proc_store):
            for i, row in enumerate(rows):
                for col, value in enumerate(row):
                    if self.proc_store[i][col] != value:
                        self.proc_store[i][col] = value
            return
        self.proc_store.clear()
        for row in rows:
            self.proc_store.append(list(row))

    # ── recommendation ───────────────────────────────────────────────────────

    def _present_on_ui(self, recommendation, context: dict):
        self._rec = recommendation
        self._context = context or {}
        self.item_store.clear()

        if recommendation is None:
            self.rec_frame.hide()
        else:
            self._fill_items(recommendation, self._context)
            self.rec_summary.set_markup(
                f"<small>{self.GLib.markup_escape_text(recommendation.summary or '')}</small>"
            )
            self.rec_frame.show()

        self.win.set_urgency_hint(True)
        self.win.present()
        self._refresh()
        return False

    def _fill_items(self, rec, context: dict):
        tabs = {t["id"]: t for t in context.get("tabs", [])}
        for tid in rec.tabs_to_close:
            t = tabs.get(tid)
            if not t:
                continue
            title = (t.get("title") or _domain(t.get("url", ""))).strip()[:70]
            self.item_store.append([
                True, title, f"{_domain(t.get('url',''))} · {_age(t.get('inactiveMinutes', 0))}",
                "tab", str(tid),
            ])

        for name in rec.processes_to_suspend:
            self.item_store.append([True, f"Suspend {name}", "process", "process", name])

        term_map = {t.pid: t for t in context.get("terminals", [])}
        for pid in rec.idle_terminals_to_close:
            t = term_map.get(pid)
            label = f"Close idle shell {t.shell_name}" if t else f"Close idle shell pid {pid}"
            self.item_store.append([True, label, f"pid {pid}", "terminal", str(pid)])

        for pw in context.get("private_windows", []):
            if not getattr(pw, "closeable", False):
                continue
            self.item_store.append([
                False, f"Close {pw.type.replace('_', ' ')}",
                f"~{pw.rss_mb:.0f} MB", "private", str(pw.xid),
            ])

        if rec.workspaces_to_sort:
            self.item_store.append([True, "Sort windows into workspaces", "", "workspace", ""])

    def _on_item_toggled(self, _renderer, path):
        self.item_store[path][0] = not self.item_store[path][0]

    def _selection(self) -> dict:
        sel: dict[str, list] = {
            "tabs": [], "processes": [], "terminals": [],
            "private": [], "workspaces": False,
        }
        for row in self.item_store:
            if not row[0]:
                continue
            kind, ident = row[3], row[4]
            if kind == "tab":
                sel["tabs"].append(int(ident))
            elif kind == "process":
                sel["processes"].append(ident)
            elif kind == "terminal":
                sel["terminals"].append(int(ident))
            elif kind == "private":
                sel["private"].append(ident)
            elif kind == "workspace":
                sel["workspaces"] = True
        sel["goal"] = self.goal_entry.get_text().strip()
        return sel

    # ── button handlers ──────────────────────────────────────────────────────

    def _on_apply(self, _btn):
        if self._busy:
            return
        sel = self._selection()
        count = len(sel["tabs"]) + len(sel["processes"]) + len(sel["terminals"]) + len(sel["private"])
        if not count and not sel["workspaces"]:
            self._set_status("nothing selected")
            return
        self._busy = True
        self.apply_btn.set_sensitive(False)
        self._set_status(f"freeing {count} item(s)…")
        self.rec_frame.hide()
        self.item_store.clear()
        self._rec = None
        try:
            self.ctl.apply_selection(sel)
        except Exception as e:
            log.warning("apply failed: %s", e)
            self.set_status_threadsafe(f"failed: {e}")

    def _on_dismiss(self, _btn):
        self.rec_frame.hide()
        self.item_store.clear()
        self._rec = None
        self._set_status("dismissed")

    def _on_analyze(self, _widget):
        goal = self.goal_entry.get_text().strip()
        self.analyze_btn.set_sensitive(False)
        self._set_status("analyzing…")

        def _reenable():
            self.analyze_btn.set_sensitive(True)
            return False

        self.GLib.timeout_add_seconds(4, _reenable)
        try:
            self.ctl.request_analysis(goal)
        except Exception as e:
            self._set_status(f"error: {e}")

    def _on_resume(self, _btn):
        try:
            self.ctl.resume_all()
            self._set_status("resumed suspended processes")
        except Exception as e:
            self._set_status(f"error: {e}")

    def _on_delete(self, _w, _e):
        self.win.hide()
        return True   # stop GTK destroying it — the daemon can present it again

    def _set_status(self, text: str):
        self.status_label.set_markup(
            f"<small>{self.GLib.markup_escape_text(text)}</small>"
        )
