"""
GTK3 confirmation dialog for RAM cleanup actions.

New in this version:
- Optional "What are you working on?" goal text entry
- Individual tab checkboxes (user can de/re-select each tab)
- Private/incognito window section with close option
- "Analyze with my goal" button re-runs Claude with the goal context
- Returns (confirmed, selected_tab_ids, private_xids_to_close, goal_text)

Falls back to CLI if GTK is unavailable.
"""
import logging
import threading
from urllib.parse import urlparse

log = logging.getLogger("ramwarden.ui")


def show_prompt(
    recommendation,
    snap,
    terminals=None,
    tabs=None,
    private_windows=None,
    re_analyze_fn=None,
) -> tuple[bool, list[int], list[str], str]:
    """
    Show the cleanup confirmation dialog.
    Returns (confirmed, selected_tab_ids, private_xids_to_close, goal_text).
    """
    try:
        import gi
        gi.require_version("Gtk", "3.0")
        from gi.repository import Gtk  # noqa: F401
        return _gtk_prompt(
            recommendation, snap,
            terminals or [], tabs or [], private_windows or [], re_analyze_fn,
        )
    except Exception as e:
        log.warning("GTK unavailable (%s) — falling back to CLI prompt", e)
        confirmed, ids = _cli_prompt(recommendation)
        return confirmed, ids, [], ""


# ── Helpers ───────────────────────────────────────────────────────────────────

def _domain(url: str) -> str:
    try:
        return urlparse(url).netloc or url[:40]
    except Exception:
        return url[:40]


def _age_label(minutes: int) -> str:
    if minutes < 60:
        return f"{minutes}m"
    elif minutes < 1440:
        return f"{minutes // 60}h"
    return f"{minutes // 1440}d"


def _private_window_label(pw) -> str:
    labels = {
        "firefox_private": "Firefox (Private Browsing)",
        "brave_incognito": "Brave (Incognito)",
        "brave_tor":       "Brave (Private Window with Tor)",
        "tor_browser":     "Tor Browser",
    }
    base = labels.get(pw.type, pw.type)
    note = "  [info only — Flatpak/Wayland, cannot close]" if not pw.closeable else ""
    return f"{base}  –  ~{pw.rss_mb:.0f} MB{note}"


# ── GTK dialog ────────────────────────────────────────────────────────────────

def _gtk_prompt(
    recommendation, snap, terminals, tabs, private_windows, re_analyze_fn
) -> tuple[bool, list[int], list[str], str]:
    import gi
    gi.require_version("Gtk", "3.0")
    from gi.repository import Gtk, GLib, GObject

    # ── Build tab list model ──────────────────────────────────────────────────
    # Columns: checked(bool), title(str), domain(str), age(str), tab_id(int)
    tab_store = Gtk.ListStore(GObject.TYPE_BOOLEAN, str, str, str, int)

    def _populate_tab_store(rec):
        tab_store.clear()
        checked_ids = set(rec.tabs_to_close)
        for t in sorted(tabs, key=lambda x: -x.get("inactiveMinutes", 0)):
            tid = t.get("id", -1)
            if tid < 0:
                continue
            checked = tid in checked_ids
            title = (t.get("title") or _domain(t.get("url", ""))).strip()[:80]
            domain = _domain(t.get("url", ""))
            age = _age_label(t.get("inactiveMinutes", 0))
            tab_store.append([checked, title, domain, age, tid])

    _populate_tab_store(recommendation)

    # ── Build private window model ─────────────────────────────────────────────
    # Columns: checked, label, xid, can_save, closeable
    # closeable=False → toggle is insensitive (grayed out, not interactive)
    priv_store = Gtk.ListStore(
        GObject.TYPE_BOOLEAN, str, str, GObject.TYPE_BOOLEAN, GObject.TYPE_BOOLEAN
    )
    for pw in private_windows:
        priv_store.append([False, _private_window_label(pw), pw.xid, pw.can_get_urls, pw.closeable])

    # ── Dialog window ─────────────────────────────────────────────────────────
    dialog = Gtk.Dialog(title="RamWarden", flags=0)
    dialog.set_keep_above(True)
    dialog.set_default_size(560, 640)
    dialog.add_button("Skip", Gtk.ResponseType.CANCEL)
    dialog.add_button("Apply", Gtk.ResponseType.OK)
    dialog.set_default_response(Gtk.ResponseType.OK)

    content = dialog.get_content_area()
    content.set_spacing(0)
    content.set_margin_top(12)
    content.set_margin_bottom(4)
    content.set_margin_start(16)
    content.set_margin_end(16)

    # ── RAM header ────────────────────────────────────────────────────────────
    header = Gtk.Label()
    header.set_markup(
        f"<b>RAM at {snap.percent:.0f}%</b>  "
        f"<span foreground='gray'>{snap.used_mb:.0f} / {snap.total_mb:.0f} MB</span>"
    )
    header.set_xalign(0)
    content.pack_start(header, False, False, 4)

    # ── Goal entry ────────────────────────────────────────────────────────────
    goal_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
    goal_entry = Gtk.Entry()
    goal_entry.set_placeholder_text("What are you working on? (optional — guides Claude)")
    goal_entry.set_hexpand(True)
    reanalyze_btn = Gtk.Button(label="Analyze with goal ▶")
    goal_box.pack_start(goal_entry, True, True, 0)
    goal_box.pack_start(reanalyze_btn, False, False, 0)
    content.pack_start(goal_box, False, False, 6)

    # Status label shown during / after re-analysis
    status_label = Gtk.Label(label="")
    status_label.set_xalign(0)
    status_label.set_no_show_all(True)
    content.pack_start(status_label, False, False, 2)

    # ── Tab list ──────────────────────────────────────────────────────────────
    tab_header = Gtk.Label()
    tab_header.set_xalign(0)
    tab_header.set_markup("<b>Browser tabs</b>  <span foreground='gray'>(✓ = will close)</span>")
    content.pack_start(tab_header, False, False, 6)

    tab_view = Gtk.TreeView(model=tab_store)
    tab_view.set_headers_visible(True)

    toggle_renderer = Gtk.CellRendererToggle()
    def _on_tab_toggle(renderer, path):
        tab_store[path][0] = not tab_store[path][0]
    toggle_renderer.connect("toggled", _on_tab_toggle)
    col_check = Gtk.TreeViewColumn("", toggle_renderer, active=0)
    col_check.set_min_width(36)
    tab_view.append_column(col_check)

    title_renderer = Gtk.CellRendererText()
    title_renderer.set_property("ellipsize", 3)
    col_title = Gtk.TreeViewColumn("Title", title_renderer, text=1)
    col_title.set_expand(True)
    tab_view.append_column(col_title)

    domain_renderer = Gtk.CellRendererText()
    domain_renderer.set_property("foreground", "gray")
    col_domain = Gtk.TreeViewColumn("Domain", domain_renderer, text=2)
    col_domain.set_min_width(110)
    tab_view.append_column(col_domain)

    age_renderer = Gtk.CellRendererText()
    age_renderer.set_property("foreground", "gray")
    col_age = Gtk.TreeViewColumn("Idle", age_renderer, text=3)
    col_age.set_min_width(44)
    tab_view.append_column(col_age)

    tab_scroll = Gtk.ScrolledWindow()
    tab_scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
    tab_scroll.set_min_content_height(220)
    tab_scroll.add(tab_view)
    content.pack_start(tab_scroll, True, True, 0)

    # ── Private / Incognito windows ───────────────────────────────────────────
    if private_windows:
        priv_header = Gtk.Label()
        priv_header.set_xalign(0)
        priv_header.set_markup("<b>Private / Incognito windows</b>")
        content.pack_start(priv_header, False, False, 8)

        priv_view = Gtk.TreeView(model=priv_store)
        priv_view.set_headers_visible(False)

        priv_toggle = Gtk.CellRendererToggle()
        def _on_priv_toggle(renderer, path):
            # Only toggle if this window is actually closeable
            if priv_store[path][4]:
                priv_store[path][0] = not priv_store[path][0]
        priv_toggle.connect("toggled", _on_priv_toggle)
        # Bind 'activatable' to column 4 (closeable) so non-closeable rows are grayed out
        priv_col = Gtk.TreeViewColumn("", priv_toggle, active=0, activatable=4)
        priv_view.append_column(priv_col)

        priv_lbl_col = Gtk.CellRendererText()
        priv_view.append_column(Gtk.TreeViewColumn("Window", priv_lbl_col, text=1))
        content.pack_start(priv_view, False, False, 0)

        has_non_closeable = any(not pw.closeable for pw in private_windows)
        has_incognito_brave = any(
            pw.type in ("brave_tor", "brave_incognito") for pw in private_windows
        )

        if has_non_closeable:
            warn = Gtk.Label()
            warn.set_markup(
                "<small><span foreground='gray'>ℹ  Brave Tor/Incognito windows run in a "
                "Flatpak sandbox — RamWarden can detect them but cannot close them from "
                "outside. Close them manually in Brave.</span></small>"
            )
            warn.set_xalign(0)
            warn.set_line_wrap(True)
            content.pack_start(warn, False, False, 2)

        if has_incognito_brave:
            incog_warn = Gtk.Label()
            incog_warn.set_markup(
                "<small><span foreground='orange'>⚠  To see and close Brave Incognito tabs: "
                "go to <b>brave://extensions → RamWarden → Details</b> and enable "
                "<b>Allow in Private Windows</b>.</span></small>"
            )
            incog_warn.set_xalign(0)
            incog_warn.set_line_wrap(True)
            content.pack_start(incog_warn, False, False, 2)

        if any(not pw.can_get_urls and pw.closeable for pw in private_windows):
            url_warn = Gtk.Label()
            url_warn.set_markup(
                "<small><span foreground='orange'>⚠  Closing a private window discards all "
                "tab URLs — enable \"Allow in Private Windows\" in extension settings first "
                "if you want to save them.</span></small>"
            )
            url_warn.set_xalign(0)
            url_warn.set_line_wrap(True)
            content.pack_start(url_warn, False, False, 2)

    # ── Claude's reasoning expander ───────────────────────────────────────────
    expander = Gtk.Expander(label="Claude's reasoning")
    analysis_label = Gtk.Label(
        label=recommendation.summary or "(no summary)", wrap=True, xalign=0
    )
    expander.add(analysis_label)
    content.pack_start(expander, False, False, 6)

    # ── Footer: process/terminal/workspace actions ────────────────────────────
    footer_parts = [f"RAM {snap.percent:.0f}%"]
    if recommendation.processes_to_suspend:
        footer_parts.append(f"suspend: {', '.join(recommendation.processes_to_suspend)}")
    idle_terms = [
        t for t in terminals
        if t.is_idle and t.pid in recommendation.idle_terminals_to_close
    ]
    if idle_terms:
        footer_parts.append(f"close {len(idle_terms)} idle terminal(s)")
    if recommendation.workspaces_to_sort:
        footer_parts.append("sort workspaces")
    footer = Gtk.Label(label="  ·  ".join(footer_parts))
    footer.set_xalign(0)
    footer.get_style_context().add_class("dim-label")
    content.pack_start(footer, False, False, 4)

    dialog.show_all()

    # ── Re-analyze callback ───────────────────────────────────────────────────
    _running = [False]

    def _bg_reanalyze(goal: str):
        try:
            if re_analyze_fn is None:
                raise RuntimeError("No analyzer available")
            new_rec = re_analyze_fn(goal)
            GLib.idle_add(_finish_reanalyze, new_rec, None)
        except Exception as exc:
            GLib.idle_add(_finish_reanalyze, None, str(exc))

    def _finish_reanalyze(new_rec, error):
        _running[0] = False
        reanalyze_btn.set_sensitive(True)
        reanalyze_btn.set_label("Analyze with goal ▶")
        if error:
            status_label.set_markup(f"<span foreground='red'>Error: {error}</span>")
            status_label.show()
        elif new_rec:
            _populate_tab_store(new_rec)
            analysis_label.set_text(new_rec.summary or "(no summary)")
            count = sum(1 for row in tab_store if row[0])
            status_label.set_markup(
                f"<span foreground='#4ade80'>✓ Re-analyzed — {count} tab(s) selected</span>"
            )
            status_label.show()

    def _on_reanalyze_clicked(_btn):
        if _running[0]:
            return
        goal = goal_entry.get_text().strip()
        if not goal:
            goal_entry.grab_focus()
            return
        _running[0] = True
        reanalyze_btn.set_sensitive(False)
        reanalyze_btn.set_label("Analyzing…")
        status_label.hide()
        threading.Thread(target=_bg_reanalyze, args=(goal,), daemon=True).start()

    reanalyze_btn.connect("clicked", _on_reanalyze_clicked)

    # ── Run ───────────────────────────────────────────────────────────────────
    response = dialog.run()
    confirmed = (response == Gtk.ResponseType.OK)
    goal_text = goal_entry.get_text().strip()

    selected_tab_ids: list[int] = []
    private_xids: list[str] = []
    if confirmed:
        for row in tab_store:
            if row[0]:
                selected_tab_ids.append(row[4])
        for row in priv_store:
            if row[0] and row[4] and row[2]:  # checked AND closeable AND has xid
                private_xids.append(row[2])

    dialog.destroy()
    while Gtk.events_pending():
        Gtk.main_iteration_do(False)

    return confirmed, selected_tab_ids, private_xids, goal_text


# ── CLI fallback ──────────────────────────────────────────────────────────────

def _cli_prompt(recommendation) -> tuple[bool, list[int]]:
    parts = []
    if recommendation.tabs_to_close:
        parts.append(f"{len(recommendation.tabs_to_close)} tab(s)")
    if recommendation.idle_terminals_to_close:
        parts.append(f"{len(recommendation.idle_terminals_to_close)} idle terminal(s)")
    if recommendation.workspaces_to_sort:
        parts.append("workspace sort")
    label = " + ".join(parts) or "cleanup"

    print(f"\n[RamWarden] Suggested: {label}")
    if recommendation.summary:
        print(f"  {recommendation.summary}")
    try:
        answer = input("Apply? [y/N] ").strip().lower()
    except (EOFError, KeyboardInterrupt):
        return False, []
    if answer == "y":
        return True, list(recommendation.tabs_to_close)
    return False, []
