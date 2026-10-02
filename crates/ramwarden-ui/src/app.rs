//! The RamWarden window.
//!
//! # What this fixes about v1's window
//!
//! v1 put the process list in a `Gtk.ScrolledWindow` with
//! `PolicyType.NEVER` for horizontal scrolling and an ellipsizing name column.
//! Long names — `openclaw-gatewa`, or any Flatpak scope stem — rendered as
//! `openclaw…` with no way to see the rest, and there was no sorting, no
//! filtering, and no way to act on a row.
//!
//! This is a `ColumnView`: every column is resizable and sortable, the view
//! scrolls horizontally, a filter box narrows 40 rows to the one you want, and
//! right-clicking a row offers the actions the daemon will actually permit.
//!
//! The window owns no policy. It renders `/state` and posts what you click; the
//! gate that decides what is allowed lives in the daemon, and a refusal comes
//! back with the gate's own reason attached.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk4::gdk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;

use crate::client::{Client, State};
use crate::model::{self, Action, Row, SortKey};
use crate::row_object::RowObject;

/// How often to re-read `/state`. v1 used four seconds and that felt right.
const REFRESH: Duration = Duration::from_secs(4);

/// Everything the window needs to keep between refreshes.
struct Ui {
    store: gio::ListStore,
    process_scroll: gtk4::ScrolledWindow,
    ram_label: gtk4::Label,
    ram_bar: gtk4::ProgressBar,
    detail: gtk4::Label,
    status: gtk4::Label,
    suspended_box: gtk4::Box,
    suspended_frame: gtk4::Frame,
    kill_banner: gtk4::Frame,
    kill_label: gtk4::Label,
    filter: gtk4::SearchEntry,
    goal: gtk4::Entry,
    rows: RefCell<Vec<Row>>,
    watchlist: RefCell<Vec<String>>,
    /// Rows the user has ticked, by pid, kept across refreshes so a selection
    /// survives the table being rebuilt underneath it.
    ticked: RefCell<Vec<i32>>,
}

fn fmt(mb: f64) -> String {
    model::fmt_mb(mb)
}

pub fn run(client: Client) -> glib::ExitCode {
    let app = gtk4::Application::builder()
        .application_id("com.system76.RamWarden")
        .build();

    let client = Rc::new(client);
    app.connect_activate(move |app| build(app, Rc::clone(&client)));
    // Our own argv is already parsed; GTK must not try again.
    app.run_with_args::<&str>(&[])
}

/// Build the real window, including its refresh loop.
///
/// Exposed so the widget tree can be asserted on: a window that starts without
/// crashing is not a window that renders correctly, and the column properties
/// the user asked about are worth checking rather than trusting.
pub fn build_for_test(app: &gtk4::Application, client: Client) {
    build(app, Rc::new(client));
}

fn build(app: &gtk4::Application, client: Rc<Client>) {
    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .title("RamWarden")
        .default_width(420)
        .default_height(600)
        .build();

    let store = gio::ListStore::new::<RowObject>();

    let ui = Rc::new(Ui {
        store: store.clone(),
        process_scroll: gtk4::ScrolledWindow::new(),
        ram_label: gtk4::Label::builder().label("Loading memory statistics…").xalign(0.0).ellipsize(gtk4::pango::EllipsizeMode::End).max_width_chars(1).build(),
        ram_bar: gtk4::ProgressBar::new(),
        detail: gtk4::Label::builder().xalign(0.0).wrap(true).wrap_mode(gtk4::pango::WrapMode::WordChar).max_width_chars(1).build(),
        status: gtk4::Label::builder().xalign(1.0).ellipsize(gtk4::pango::EllipsizeMode::End).build(),
        suspended_box: gtk4::Box::new(gtk4::Orientation::Vertical, 4),
        suspended_frame: gtk4::Frame::new(None),
        kill_banner: gtk4::Frame::new(None),
        kill_label: gtk4::Label::builder().xalign(0.0).wrap(true).build(),
        filter: gtk4::SearchEntry::builder()
            .placeholder_text("Filter processes")
            .hexpand(true)
            .build(),
        goal: gtk4::Entry::builder()
            .placeholder_text("What are you working on? (optional)")
            .hexpand(true)
            .build(),
        rows: RefCell::new(Vec::new()),
        watchlist: RefCell::new(Vec::new()),
        ticked: RefCell::new(Vec::new()),
    });

    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    outer.set_margin_top(12);
    outer.set_margin_bottom(12);
    outer.set_margin_start(14);
    outer.set_margin_end(14);

    // ── memory pressure ─────────────────────────────────────────────────────
    outer.append(&ui.ram_label);
    ui.ram_bar.set_show_text(false);
    outer.append(&ui.ram_bar);
    ui.detail.add_css_class("dim-label");
    outer.append(&ui.detail);

    // ── an armed kill, which must be impossible to miss ─────────────────────
    ui.kill_banner.add_css_class("error");
    let kill_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    kill_row.set_margin_top(6);
    kill_row.set_margin_bottom(6);
    kill_row.set_margin_start(8);
    kill_row.set_margin_end(8);
    ui.kill_label.set_hexpand(true);
    kill_row.append(&ui.kill_label);
    let cancel = gtk4::Button::with_label("Cancel");
    cancel.add_css_class("destructive-action");
    kill_row.append(&cancel);
    ui.kill_banner.set_child(Some(&kill_row));
    outer.append(&ui.kill_banner);
    ui.kill_banner.set_visible(false);

    {
        let (c, u) = (Rc::clone(&client), Rc::clone(&ui));
        cancel.connect_clicked(move |_| {
            let (c, u) = (Rc::clone(&c), Rc::clone(&u));
            glib::spawn_future_local(async move {
                match c.cancel_kill().await {
                    Ok(true) => set_status(&u, "kill cancelled"),
                    Ok(false) => set_status(&u, "nothing was armed"),
                    Err(e) => set_status(&u, &format!("cancel failed: {e}")),
                }
            });
        });
    }

    // ── suspended banner ────────────────────────────────────────────────────
    // A SIGSTOPped GUI app is indistinguishable from a crashed one. Without this
    // the only clue RamWarden froze something is that it stopped responding,
    // which reads as a bug in the app and gets it force-quit.
    ui.suspended_frame.add_css_class("app-notification");
    let sb = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    sb.set_margin_top(6);
    sb.set_margin_bottom(6);
    sb.set_margin_start(8);
    sb.set_margin_end(8);
    let title = gtk4::Label::builder().xalign(0.0).build();
    title.set_markup("<b>\u{23f8}  Suspended by RamWarden</b>");
    sb.append(&title);
    sb.append(&ui.suspended_box);
    ui.suspended_frame.set_child(Some(&sb));
    outer.append(&ui.suspended_frame);
    ui.suspended_frame.set_visible(false);

    // ── filter ──────────────────────────────────────────────────────────────
    let filter_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    filter_row.append(&ui.filter);
    let legend = gtk4::Label::new(None);
    legend.set_markup(
        "<small><span foreground='#60a5fa'>■</span> protected  \
         <span foreground='#4ade80'>■</span> in use  \
         <span foreground='#facc15'>■</span> idle</small>",
    );
    legend.set_xalign(0.0);
    outer.append(&filter_row);

    // ── the table ───────────────────────────────────────────────────────────
    // The view owns the ordering: a `SortListModel` driven by the view's own
    // sorter means header clicks work, the arrow appears, and the sort survives
    // the store being rebuilt on every refresh.
    let view = gtk4::ColumnView::builder()
        .show_column_separators(true)
        .show_row_separators(false)
        .hexpand(true)
        .vexpand(true)
        .build();

    let sorted = gtk4::SortListModel::new(Some(store.clone()), view.sorter());
    view.set_model(Some(&gtk4::NoSelection::new(Some(sorted))));

    add_columns(&view, &ui, &client);

    // Start on the largest consumer, which is what the user is looking for.
    if let Some(ram) = (0..view.columns().n_items()).find_map(|i| {
        view.columns()
            .item(i)
            .and_downcast::<gtk4::ColumnViewColumn>()
            .filter(|c| c.title().is_some_and(|t| t == "RAM"))
    }) {
        view.sort_by_column(Some(&ram), gtk4::SortType::Descending);
    }

    let scroll = &ui.process_scroll;
    scroll.set_policy(gtk4::PolicyType::Automatic, gtk4::PolicyType::Automatic);
    scroll.set_min_content_height(150);
    scroll.set_hexpand(true);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&view));
    outer.append(scroll);
    outer.append(&legend);

    // ── bulk actions on ticked rows ─────────────────────────────────────────
    let bulk = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    for (label, action) in [
        ("Reclaim selected", Action::Reclaim),
        ("Suspend selected", Action::Suspend),
        ("Resume selected", Action::Resume),
        ("Kill selected", Action::Kill),
    ] {
        let b = gtk4::Button::with_label(label.trim_end_matches(" selected"));
        b.set_tooltip_text(Some(label));
        b.set_hexpand(true);
        if action.destructive() {
            b.add_css_class("destructive-action");
        }
        let (c, u) = (Rc::clone(&client), Rc::clone(&ui));
        b.connect_clicked(move |_| bulk_apply(Rc::clone(&c), Rc::clone(&u), action));
        bulk.append(&b);
    }
    outer.append(&bulk);

    let browser = gtk4::Expander::builder().label("Browser tabs · suggested cleanup").expanded(true).build();
    browser.set_widget_name("browser-cleanup");
    browser.set_child(Some(&crate::browser::pane((*client).clone())));
    outer.append(&browser);

    // ── goal + analyse ──────────────────────────────────────────────────────
    let goal_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    goal_row.append(&ui.goal);
    let analyse = gtk4::Button::with_label("Analyse");
    analyse.add_css_class("suggested-action");
    goal_row.append(&analyse);
    outer.append(&goal_row);

    {
        let (c, u) = (Rc::clone(&client), Rc::clone(&ui));
        let button = analyse.clone();
        let run_analysis = move || {
            if !button.is_sensitive() {
                return;
            }
            button.set_sensitive(false);
            let button = button.clone();
            let (c, u) = (Rc::clone(&c), Rc::clone(&u));
            let goal = u.goal.text().to_string();
            set_status(&u, "analysing…");
            tracing::info!("analysis started");
            glib::spawn_future_local(async move {
                match c.analyze(&goal).await {
                    Ok(v) => {
                        let tier = v.get("tier").and_then(|t| t.as_str()).unwrap_or("?");
                        let summary = v.get("summary").and_then(|s| s.as_str()).unwrap_or("");
                        let n = v.get("tabs_to_close").and_then(|t| t.as_array()).map_or(0, |a| a.len());
                        tracing::info!(tier, tabs = n, "analysis completed");
                        set_status(&u, &format!("[{tier}] {summary} ({n} tab(s))"));
                    }
                    Err(e) => {
                        tracing::warn!("analysis failed: {e}");
                        set_status(&u, &format!("analysis failed: {e}"));
                    }
                }
                button.set_sensitive(true);
            });
        };
        analyse.connect_clicked({
            let f = run_analysis.clone();
            move |_| f()
        });
        ui.goal.connect_activate(move |_| run_analysis());
    }

    // ── status line ─────────────────────────────────────────────────────────
    let foot = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    ui.status.set_hexpand(true);
    ui.status.add_css_class("dim-label");
    foot.append(&ui.status);
    outer.append(&foot);

    // Re-filtering is cheap and local, so it happens without touching the daemon.
    {
        let u = Rc::clone(&ui);
        ui.filter.connect_search_changed(move |_| repopulate(&u));
    }

    window.set_child(Some(&outer));
    window.present();

    // ── refresh loop ────────────────────────────────────────────────────────
    let (c, u) = (Rc::clone(&client), Rc::clone(&ui));
    glib::spawn_future_local(async move {
        // Pull the watchlist once; it changes only when the user edits it.
        if let Ok(w) = c.watchlist().await {
            *u.watchlist.borrow_mut() = w;
        }
    });
    let (c, u) = (Rc::clone(&client), Rc::clone(&ui));
    glib::spawn_future_local(async move {
        loop {
            refresh(&c, &u).await;
            glib::timeout_future(REFRESH).await;
        }
    });
}

/// A comparator for one column, in GTK's own sorter form.
///
/// GTK inverts it for a descending sort and paints the header arrow itself, so
/// each sorter only has to express the ascending order. Ties break on pid so rows
/// do not shuffle between refreshes while the user is trying to click one.
fn sorter_for(key: SortKey) -> gtk4::CustomSorter {
    gtk4::CustomSorter::new(move |a, b| {
        let (Some(a), Some(b)) = (
            a.downcast_ref::<RowObject>().map(|o| o.row()),
            b.downcast_ref::<RowObject>().map(|o| o.row()),
        ) else {
            return gtk4::Ordering::Equal;
        };
        let ord = match key {
            SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortKey::Pid => a.pid.cmp(&b.pid),
            SortKey::Memory => a.pss_mb.total_cmp(&b.pss_mb),
            SortKey::Vram => a.vram_mb.total_cmp(&b.vram_mb),
            SortKey::Status => a.verdict.cmp(&b.verdict),
            SortKey::Scope => a
                .scope_label()
                .to_lowercase()
                .cmp(&b.scope_label().to_lowercase()),
        };
        ord.then_with(|| a.pid.cmp(&b.pid)).into()
    })
}

/// Build the columns, each with its own width and sort behaviour.
fn add_columns(view: &gtk4::ColumnView, ui: &Rc<Ui>, client: &Rc<Client>) {
    for spec in model::COLUMNS {
        let factory = gtk4::SignalListItemFactory::new();

        match spec.title {
            // The tick column drives the bulk toolbar.
            "" => {
                let u = Rc::clone(ui);
                factory.connect_setup(|_, item| {
                    let check = gtk4::CheckButton::new();
                    item.downcast_ref::<gtk4::ListItem>().unwrap().set_child(Some(&check));
                });
                factory.connect_bind(move |_, item| {
                    let item = item.downcast_ref::<gtk4::ListItem>().unwrap();
                    let Some(obj) = item.item().and_downcast::<RowObject>() else { return };
                    let Some(check) = item.child().and_downcast::<gtk4::CheckButton>() else { return };
                    let row = obj.row();
                    // A structurally protected row can never be acted on, so it
                    // cannot be ticked either.
                    check.set_sensitive(row.actionable());
                    check.set_active(u.ticked.borrow().contains(&row.pid));

                    let (u2, pid) = (Rc::clone(&u), row.pid);
                    // Replace the handler on each bind; the previous one belonged to
                    // whichever row was recycled into this widget.
                    check.connect_toggled(move |c| {
                        let mut ticked = u2.ticked.borrow_mut();
                        if c.is_active() {
                            if !ticked.contains(&pid) {
                                ticked.push(pid);
                            }
                        } else {
                            ticked.retain(|p| *p != pid);
                        }
                    });
                });
            }
            title => {
                let title = title.to_string();
                factory.connect_setup(|_, item| {
                    let label = gtk4::Label::builder()
                        .xalign(0.0)
                        .ellipsize(gtk4::pango::EllipsizeMode::None)
                        .build();
                    item.downcast_ref::<gtk4::ListItem>().unwrap().set_child(Some(&label));
                });
                factory.connect_bind(move |_, item| {
                    let item = item.downcast_ref::<gtk4::ListItem>().unwrap();
                    let Some(obj) = item.item().and_downcast::<RowObject>() else { return };
                    let Some(label) = item.child().and_downcast::<gtk4::Label>() else { return };
                    let row = obj.row();
                    match title.as_str() {
                        "Process" => {
                            label.set_text(&row.name);
                            label.set_tooltip_text(Some(&row.all_reasons()));
                        }
                        "PID" => {
                            label.set_text(&row.pid.to_string());
                            label.set_xalign(1.0);
                        }
                        "RAM" => {
                            label.set_text(&fmt(row.pss_mb));
                            label.set_xalign(1.0);
                            // Showing how far a naive RSS reading would have been
                            // out is the clearest way to explain the rewrite.
                            if row.overstatement() > 1.2 {
                                label.set_tooltip_text(Some(&format!(
                                    "PSS {}\nsummed RSS would claim {} ({:.1}x)",
                                    fmt(row.pss_mb),
                                    fmt(row.true_rss_mb),
                                    row.overstatement()
                                )));
                            }
                        }
                        "VRAM" => {
                            let text = if row.vram_mb > 0.0 {
                                fmt(row.vram_mb)
                            } else {
                                "—".to_string()
                            };
                            label.set_text(&text);
                            label.set_xalign(1.0);
                        }
                        "Status" => {
                            label.set_markup(&format!(
                                "<span foreground='{}'>{}</span>",
                                model::verdict_colour(&row.verdict),
                                glib::markup_escape_text(&row.verdict)
                            ));
                        }
                        "Scope" => label.set_text(&row.scope_label()),
                        _ => {
                            label.set_text(row.reason());
                            label.set_tooltip_text(Some(&row.all_reasons()));
                        }
                    }
                });
            }
        }

        let column = gtk4::ColumnViewColumn::builder()
            .title(spec.title)
            .factory(&factory)
            .resizable(spec.resizable)
            .expand(spec.expand)
            .fixed_width(spec.min_width)
            .build();

        // Clicking a header sorts. `ColumnViewColumn` has no click signal; the
        // supported route is to give it a sorter and let the view drive a
        // `SortListModel`, which is what `sorted_model` below wires up. Doing it
        // with a hand-rolled gesture would fight GTK's own header handling and
        // lose the sort indicator arrow.
        if let Some(key) = spec.sort {
            column.set_sorter(Some(&sorter_for(key)));
        }

        view.append_column(&column);
    }

    // ── right-click ─────────────────────────────────────────────────────────
    let gesture = gtk4::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    let (u, c, v) = (Rc::clone(ui), Rc::clone(client), view.clone());
    gesture.connect_pressed(move |_, _, x, y| {
        if let Some(row) = row_at(&v, &u, y) {
            show_menu(&v, &u, &c, &row, x, y);
        }
    });
    view.add_controller(gesture);
}

/// Which row is under a click.
///
/// Read from the view's own model, not the backing store: the store is in
/// insertion order while the view shows it sorted, so indexing the store would
/// hand back the wrong row the moment anyone sorts by a column.
fn row_at(view: &gtk4::ColumnView, _ui: &Rc<Ui>, y: f64) -> Option<Row> {
    let model = view.model()?;
    let n = model.n_items();
    if n == 0 {
        return None;
    }
    // Row height is uniform, so the index follows from the offset.
    let height = view.height() as f64;
    if height <= 0.0 {
        return None;
    }
    let per_row = (height / n as f64).max(1.0);
    let idx = ((y / per_row).floor() as u32).min(n - 1);
    model.item(idx).and_downcast::<RowObject>().map(|o| o.row())
}

/// The context menu for one row.
fn show_menu(
    view: &gtk4::ColumnView,
    ui: &Rc<Ui>,
    client: &Rc<Client>,
    row: &Row,
    x: f64,
    y: f64,
) {
    let watchlisted = ui
        .watchlist
        .borrow()
        .iter()
        .any(|w| w.eq_ignore_ascii_case(&row.name));

    let list = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let header = gtk4::Label::new(None);
    header.set_markup(&format!(
        "<b>{}</b>  <span foreground='gray'>pid {}</span>",
        glib::markup_escape_text(&row.name),
        row.pid
    ));
    header.set_xalign(0.0);
    list.append(&header);

    if row.is_structural() {
        let why = gtk4::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(44)
            .build();
        why.set_markup(&format!(
            "<small>{}</small>",
            glib::markup_escape_text(row.reason())
        ));
        why.add_css_class("dim-label");
        list.append(&why);
    }

    let popover = gtk4::Popover::builder().child(&list).build();
    popover.set_parent(view);
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.set_position(gtk4::PositionType::Bottom);

    for item in model::menu_for(row, watchlisted) {
        let button = gtk4::Button::builder()
            .label(item.label())
            .has_frame(false)
            .build();
        if let Some(child) = button.child().and_downcast::<gtk4::Label>() {
            child.set_xalign(0.0);
        }
        if item.action.destructive() {
            button.add_css_class("destructive-action");
        }

        // A soft-protected row — one holding a listening socket — is reachable,
        // but the consequence travels with the offer.
        if let Some(warning) = &item.warning {
            button.set_tooltip_text(Some(warning));
        }

        let (c, u, r, act) = (
            Rc::clone(client), Rc::clone(ui), row.clone(), item.action,
        );
        let warning = item.warning.clone();
        let pop = popover.clone();
        button.connect_clicked(move |_| {
            pop.popdown();
            apply(Rc::clone(&c), Rc::clone(&u), r.clone(), act, warning.clone());
        });
        list.append(&button);
    }

    popover.popup();
}

/// Carry out one action, or ask first when it cannot be undone.
fn apply(
    client: Rc<Client>,
    ui: Rc<Ui>,
    row: Row,
    action: Action,
    warning: Option<String>,
) {
    if action == Action::CopyPid {
        if let Some(display) = gdk::Display::default() {
            display.clipboard().set_text(&row.pid.to_string());
        }
        set_status(&ui, &format!("copied pid {}", row.pid));
        return;
    }

    // Autonomy is the ladder's business. A click is the user's, and an
    // irreversible one gets a question first.
    if action.destructive() || warning.is_some() {
        confirm(client, ui, row, action, warning);
        return;
    }
    perform(client, ui, row, action, false);
}

fn confirm(
    client: Rc<Client>,
    ui: Rc<Ui>,
    row: Row,
    action: Action,
    warning: Option<String>,
) {
    let detail = match (&warning, action) {
        (Some(w), _) => format!("{w}\n\nThis overrides RamWarden's protection."),
        (None, Action::Kill) => {
            "Unsaved work in this application will be lost.".to_string()
        }
        _ => String::new(),
    };

    let dialog = gtk4::AlertDialog::builder()
        .message(format!("{} {}?", action.label(), row.name))
        .detail(detail)
        .buttons(["Cancel", action.label()])
        .cancel_button(0)
        .default_button(0)
        .modal(true)
        .build();

    let root = ui.status.root().and_downcast::<gtk4::Window>();
    dialog.choose(root.as_ref(), None::<&gio::Cancellable>, move |answer| {
        if answer == Ok(1) {
            // Confirmed explicitly, so force past the soft protections — which
            // is what "yes, that one" means. Structural protection is still
            // refused by the daemon.
            perform(client, ui, row, action, true);
        }
    });
}

fn perform(client: Rc<Client>, ui: Rc<Ui>, row: Row, action: Action, force: bool) {
    set_status(&ui, &format!("{}: {}…", row.name, action.label()));
    glib::spawn_future_local(async move {
        let text = match action {
            Action::Suspend => match client.suspend(&row.name, force).await {
                Ok(r) => r.describe("froze"),
                Err(e) => format!("suspend failed: {e}"),
            },
            Action::Resume => match client.resume(&row.name).await {
                Ok(r) => r.describe("resumed"),
                Err(e) => format!("resume failed: {e}"),
            },
            Action::Kill => match client.kill(&row.name, force).await {
                Ok(r) => r.describe("killed"),
                Err(e) => format!("kill failed: {e}"),
            },
            Action::Reclaim => match row.scope.clone() {
                Some(scope) => match client.reclaim(&scope).await {
                    Ok(v) => {
                        let freed = v.get("freed_mb").and_then(|f| f.as_f64()).unwrap_or(0.0);
                        if freed > 0.0 {
                            format!("reclaimed {} from {}", fmt(freed), row.name)
                        } else {
                            format!("{}: nothing was reclaimable", row.name)
                        }
                    }
                    Err(e) => format!("reclaim failed: {e}"),
                },
                // Only processes inside the user's own session have a scope, and
                // only those can be reclaimed from.
                None => format!("{} is outside your session — nothing to reclaim", row.name),
            },
            Action::Watch | Action::Spare => {
                "editing the watchlist is not wired up yet".to_string()
            }
            Action::CopyPid => unreachable!("handled before dispatch"),
        };
        set_status(&ui, &text);
    });
}

/// Apply one action to every ticked row.
fn bulk_apply(client: Rc<Client>, ui: Rc<Ui>, action: Action) {
    let ticked: Vec<i32> = ui.ticked.borrow().clone();
    if ticked.is_empty() {
        set_status(&ui, "nothing selected");
        return;
    }
    let rows: Vec<Row> = ui
        .rows
        .borrow()
        .iter()
        .filter(|r| ticked.contains(&r.pid) && r.actionable())
        .cloned()
        .collect();
    if rows.is_empty() {
        set_status(&ui, "every selected row is protected");
        return;
    }

    // One dialog for the whole batch rather than one per row.
    if action.destructive() {
        let names: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
        let dialog = gtk4::AlertDialog::builder()
            .message(format!("{} {} process(es)?", action.label(), rows.len()))
            .detail(format!(
                "{}\n\nUnsaved work in these applications will be lost.",
                names.join(", ")
            ))
            .buttons(["Cancel", action.label()])
            .cancel_button(0)
            .default_button(0)
            .modal(true)
            .build();
        let root = ui.status.root().and_downcast::<gtk4::Window>();
        dialog.choose(root.as_ref(), None::<&gio::Cancellable>, move |answer| {
            if answer == Ok(1) {
                for row in rows {
                    perform(Rc::clone(&client), Rc::clone(&ui), row, action, true);
                }
            }
        });
        return;
    }

    for row in rows {
        perform(Rc::clone(&client), Rc::clone(&ui), row, action, false);
    }
}

fn set_status(ui: &Rc<Ui>, text: &str) {
    ui.status
        .set_markup(&format!("<small>{}</small>", glib::markup_escape_text(text)));
}

/// Re-filter the store from the rows we already have.
///
/// Ordering is not applied here: the `SortListModel` wrapping this store keeps
/// whatever column the user clicked, so rebuilding the contents does not reset
/// their sort.
fn repopulate(ui: &Rc<Ui>) {
    let rows = ui.rows.borrow().clone();
    let filtered = model::filter(&rows, &ui.filter.text());

    let vertical = ui.process_scroll.vadjustment();
    let horizontal = ui.process_scroll.hadjustment();
    let (y, x) = (vertical.value(), horizontal.value());
    let objects: Vec<_> = filtered.into_iter().map(RowObject::new).collect();
    ui.store.splice(0, ui.store.n_items(), &objects);
    // ColumnView recomputes adjustments during layout, after idle callbacks.
    // Restore on the next completed layout rather than before it resets them.
    let frames=std::cell::Cell::new(0);
    ui.process_scroll.add_tick_callback(move |_, _| {
        frames.set(frames.get() + 1);
        if frames.get() < 2 { return glib::ControlFlow::Continue; }
        vertical.set_value(y.min((vertical.upper() - vertical.page_size()).max(0.0)));
        horizontal.set_value(x.min((horizontal.upper() - horizontal.page_size()).max(0.0)));
        glib::ControlFlow::Break
    });
}

async fn refresh(client: &Rc<Client>, ui: &Rc<Ui>) {
    let state = match client.state().await {
        Ok(s) => s,
        Err(e) => {
            // Keep the last sample visible, but clearly identify it as stale.
            tracing::warn!("state refresh failed: {e}");
            ui.ram_label.set_text("Memory statistics unavailable (last sample may be stale)");
            ui.detail
                .set_markup(&format!("<small>daemon unreachable: {}</small>",
                    glib::markup_escape_text(&e.to_string())));
            return;
        }
    };

    tracing::debug!(processes = state.processes.len(), percent = state.percent, "state refreshed");
    render_header(ui, &state);
    render_suspended(ui, client, &state);

    *ui.rows.borrow_mut() = state.processes.clone();
    repopulate(ui);

    // An armed kill is the one thing that must never be missed.
    match client.plan().await {
        Ok(plan) => match plan.kill_pending {
            Some(p) => {
                ui.kill_label.set_markup(&format!(
                    "<b>Killing {} in {}s</b>",
                    glib::markup_escape_text(&p.targets.join(", ")),
                    p.seconds_remaining
                ));
                ui.kill_banner.set_visible(true);
            }
            None => ui.kill_banner.set_visible(false),
        },
        Err(_) => ui.kill_banner.set_visible(false),
    }
}

fn render_header(ui: &Rc<Ui>, state: &State) {
    let colour = if state.percent < state.warn_percent {
        "#4ade80"
    } else if state.percent < state.warn_percent + 10.0 {
        "#facc15"
    } else {
        "#f87171"
    };
    ui.ram_label.set_markup(&format!(
        "<span size='x-large' weight='bold' foreground='{colour}'>{:.0}%</span>  \
         <span foreground='gray'>{} of {}</span>",
        state.percent,
        fmt(state.used_mb),
        fmt(state.total_mb)
    ));
    ui.ram_bar.set_fraction((state.percent / 100.0).clamp(0.0, 1.0));

    if state.warm {
        ui.detail.set_markup(&format!(
            "<small>protected {}  ·  in use {}  ·  idle {}  ·  \
             PSI {:.2}%  ·  zram saved {}  ·  {} browser(s)</small>",
            fmt(state.totals_mb.protected),
            fmt(state.totals_mb.in_use),
            fmt(state.totals_mb.idle),
            state.psi_some,
            fmt(state.zram_saved_mb),
            state.browsers_connected
        ));
    } else {
        // Before two samples nothing can honestly be called idle, and the window
        // must not imply otherwise.
        ui.detail.set_markup(
            "<small>sampling — no verdicts yet (the detector needs two readings)</small>",
        );
    }
}

fn render_suspended(ui: &Rc<Ui>, client: &Rc<Client>, state: &State) {
    while let Some(child) = ui.suspended_box.first_child() {
        ui.suspended_box.remove(&child);
    }
    if state.suspended.is_empty() {
        ui.suspended_frame.set_visible(false);
        return;
    }

    for entry in &state.suspended {
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let label = gtk4::Label::builder().xalign(0.0).hexpand(true).build();
        label.set_markup(&format!(
            "<b>{}</b>  <span foreground='gray'>{} · frozen {:.0} min</span>",
            glib::markup_escape_text(&entry.name),
            fmt(entry.rss_mb),
            entry.minutes
        ));
        row.append(&label);

        let button = gtk4::Button::with_label("Resume");
        let (c, u, name) = (Rc::clone(client), Rc::clone(ui), entry.name.clone());
        button.connect_clicked(move |_| {
            let (c, u, name) = (Rc::clone(&c), Rc::clone(&u), name.clone());
            glib::spawn_future_local(async move {
                let text = match c.resume(&name).await {
                    Ok(r) => r.describe("resumed"),
                    Err(e) => format!("resume failed: {e}"),
                };
                set_status(&u, &text);
            });
        });
        row.append(&button);
        ui.suspended_box.append(&row);
    }

    let note = gtk4::Label::builder().xalign(0.0).wrap(true).build();
    note.set_markup(
        "<small>Frozen applications cannot respond or quit until resumed. \
         RamWarden wakes them automatically once pressure passes.</small>",
    );
    note.add_css_class("dim-label");
    ui.suspended_box.append(&note);
    ui.suspended_frame.set_visible(true);
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};

    fn widgets(root: &gtk4::Widget) -> Vec<gtk4::Widget> {
        let mut out = vec![root.clone()];
        let mut child = root.first_child();
        while let Some(w) = child {
            out.extend(widgets(&w));
            child = w.next_sibling();
        }
        out
    }

    async fn until(mut predicate: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while !predicate() {
            assert!(std::time::Instant::now() < deadline, "GTK display did not reach the expected state");
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    }

    #[test]
    fn live_window_renders_refreshes_and_finishes_analysis() {
        gtk4::init().expect("run this test under xvfb-run");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let fail = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(AtomicUsize::new(0));
        let analyses = Arc::new(AtomicUsize::new(0));
        let worker = {
            let (stop, fail, samples, analyses) = (stop.clone(), fail.clone(), samples.clone(), analyses.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                    let mut buf = [0; 8192];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);
                    let path = request.split_whitespace().nth(1).unwrap_or("");
                    let failed = fail.load(Ordering::SeqCst);
                    let body = match path {
                        "/state" => {
                            samples.fetch_add(1, Ordering::SeqCst);
                            r#"{"percent":42,"used_mb":4200,"total_mb":10000,"warn_percent":80,"warm":true,"processes":[{"pid":123,"name":"fixture-app","rss_mb":250,"verdict":"IN_USE"}]}"#
                        }
                        "/analyze" => {
                            analyses.fetch_add(1, Ordering::SeqCst);
                            r#"{"tier":"heuristic","summary":"Fixture analysis complete","tabs_to_close":[]}"#
                        }
                        "/browser/tabs" | "/browser/analyze" => r#"{"browsers_connected":1,"candidates":1,"tabs":[{"browser":"fixture-browser","id":7,"url":"https://example.org/article","title":"Fixture browser tab","inactive_minutes":120,"eligible":true,"status":"candidate","reason":"idle background tab","priority":120}]}"#,
                        "/browser/discard" => r#"{"confirmed":[{"browser":"fixture-browser","id":7,"url":"https://example.org/article"}],"queued":[],"refused":[]}"#,
                        "/watchlist" => r#"{"patterns":[]}"#,
                        _ => "{}",
                    };
                    let body = if path == "/state" {
                        let mut value: serde_json::Value = serde_json::from_str(body).unwrap();
                        value["processes"] = serde_json::json!((0..50).map(|i| serde_json::json!({
                            "pid":123+i,"name":format!("fixture-app-{i}"),"rss_mb":250+i,"verdict":"IN_USE"
                        })).collect::<Vec<_>>());
                        value.to_string()
                    } else { body.to_string() };
                    std::thread::sleep(Duration::from_millis(50));
                    let status = if failed { "503 Service Unavailable" } else { "200 OK" };
                    let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                }
            })
        };
        let runtime = crate::runtime::network_runtime().unwrap();
        let _entered = runtime.enter();
        let app = gtk4::Application::builder().application_id("com.system76.RamWardenRegression").build();
        app.register(None::<&gio::Cancellable>).unwrap();
        build_for_test(&app, Client::new("127.0.0.1", port).unwrap());
        let window = app.windows()[0].clone();
        let tree = widgets(window.upcast_ref());
        let labels: Vec<_> = tree.iter().filter_map(|w| w.clone().downcast::<gtk4::Label>().ok()).collect();
        let has_text = |needle: &str| labels.iter().any(|l| l.text().contains(needle));
        let analyse = tree.iter().filter_map(|w| w.clone().downcast::<gtk4::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Analyse")).unwrap();
        let view = tree.iter().find_map(|w| w.clone().downcast::<gtk4::ColumnView>().ok()).unwrap();
        glib::MainContext::default().block_on(async {
            until(|| has_text("42%") && view.model().unwrap().n_items() == 50).await;
            assert!(has_text("PSI"));
            let process_scroll=tree.iter().filter_map(|w|w.clone().downcast::<gtk4::ScrolledWindow>().ok())
                .find(|s|s.child().is_some_and(|c|c.is::<gtk4::ColumnView>())).unwrap();
            glib::timeout_future(Duration::from_millis(150)).await;
            until(||process_scroll.vadjustment().upper()-process_scroll.vadjustment().page_size()>150.0).await;
            process_scroll.vadjustment().set_value(150.0);
            assert_eq!(process_scroll.vadjustment().value(),150.0);
            let samples_before=samples.load(Ordering::SeqCst);
            until(||samples.load(Ordering::SeqCst)>samples_before).await;
            glib::timeout_future(Duration::from_millis(150)).await;
            assert!((process_scroll.vadjustment().value()-150.0).abs()<1.0,"process scroll reset on refresh: {}",process_scroll.vadjustment().value());
            analyse.emit_clicked();
            analyse.emit_clicked();
            until(|| has_text("Fixture analysis complete")).await;
            assert!(analyse.is_sensitive());
            assert_eq!(analyses.load(Ordering::SeqCst), 1, "duplicate analysis was submitted");
            fail.store(true, Ordering::SeqCst);
            analyse.emit_clicked();
            until(|| has_text("analysis failed")).await;
            assert!(analyse.is_sensitive());
            until(|| has_text("daemon unreachable")).await;
            fail.store(false, Ordering::SeqCst);
            until(|| has_text("PSI")).await;
            assert!(samples.load(Ordering::SeqCst) >= 3);
            assert_eq!(window.default_width(), 420);
            assert!(window.width() <= 440, "window forced wider: {}", window.width());
            let pane=tree.iter().find(|w|w.widget_name()=="browser-cleanup").unwrap().clone();
            let browser_text = |needle: &str| widgets(&pane).iter().filter_map(|w|w.clone().downcast::<gtk4::Label>().ok()).any(|l|l.text().contains(needle));
            until(|| browser_text("Fixture browser tab")).await;
            let check=widgets(&pane).iter().find_map(|w|w.clone().downcast::<gtk4::CheckButton>().ok()).unwrap();
            check.set_active(true);
            let unload=widgets(&pane).iter().filter_map(|w|w.clone().downcast::<gtk4::Button>().ok())
                .find(|b|b.label().is_some_and(|l|l == "Unload")).unwrap();
            assert!(unload.is_sensitive());unload.emit_clicked();
            until(||browser_text("1 unloaded")).await;
            assert!(!unload.is_sensitive());
            crate::browser::test_refresh_stability().await;
        });
        window.close();
        stop.store(true, Ordering::SeqCst);
        worker.join().unwrap();
    }
}
