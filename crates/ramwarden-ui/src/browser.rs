//! Browser recommendations and explicit, bounded unloading.
use crate::client::{BrowserState, Client, TabTarget};
use gtk4::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
    time::Duration,
};

struct TabWidgets {
    line: gtk4::Box,
    check: gtk4::CheckButton,
    title: gtk4::Label,
    detail: gtk4::Label,
}
struct Ui {
    summary: gtk4::Label,
    result: gtk4::Label,
    list: gtk4::Box,
    refresh: gtk4::Button,
    unload: gtk4::Button,
    close: gtk4::Button,
    search: gtk4::SearchEntry,
    state: RefCell<BrowserState>,
    selected: RefCell<HashSet<TabTarget>>,
    busy: Cell<bool>,
    syncing: Cell<bool>,
    scroll: gtk4::ScrolledWindow,
    widgets: RefCell<HashMap<TabTarget, TabWidgets>>,
}

pub fn pane(client: Client) -> gtk4::Box {
    build(client, true).0
}
fn build(client: Client, poll: bool) -> (gtk4::Box, Rc<Ui>) {
    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    outer.set_margin_top(4);
    outer.set_margin_bottom(0);
    outer.set_margin_start(0);
    outer.set_margin_end(0);
    let ui = Rc::new(Ui {
        summary: gtk4::Label::builder()
            .label("Loading browser tabs…")
            .xalign(0.0)
            .wrap(true)
            .build(),
        result: gtk4::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .max_width_chars(1)
            .selectable(true)
            .build(),
        list: gtk4::Box::new(gtk4::Orientation::Vertical, 2),
        refresh: gtk4::Button::with_label("Refresh"),
        unload: gtk4::Button::with_label("Unload"),
        close: gtk4::Button::with_label("Close"),
        search: gtk4::SearchEntry::builder()
            .placeholder_text("Filter tabs")
            .build(),
        state: RefCell::new(BrowserState::default()),
        selected: RefCell::new(HashSet::new()),
        busy: Cell::new(false),
        syncing: Cell::new(false),
        scroll: gtk4::ScrolledWindow::builder()
            .height_request(120)
            .vexpand(false)
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .build(),
        widgets: RefCell::new(HashMap::new()),
    });
    outer.append(&ui.summary);
    ui.unload.set_tooltip_text(Some("Unload up to five selected tabs; they remain open and reload when selected. Pin important unsaved forms. Per-tab RAM savings are not measured."));
    let tools = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    ui.search.set_width_chars(8);
    ui.search.set_hexpand(true);
    tools.append(&ui.search);
    tools.append(&ui.refresh);
    tools.append(&ui.unload);
    ui.close.add_css_class("destructive-action");
    ui.close.set_tooltip_text(Some(
        "Close selected tabs (asks for confirmation). Unsaved work can be lost.",
    ));
    ui.close.set_sensitive(false);
    tools.append(&ui.close);
    outer.append(&tools);
    ui.unload.set_sensitive(false);
    ui.scroll.set_child(Some(&ui.list));
    outer.append(&ui.scroll);
    outer.append(&ui.result);
    {
        let (u, c) = (ui.clone(), client.clone());
        ui.refresh.connect_clicked(move |_| {
            if u.busy.replace(true) {
                return;
            }
            controls(&u);
            let (u, c) = (u.clone(), c.clone());
            glib::spawn_future_local(async move {
                u.result.set_text("Reading fresh browser activity…");
                match c.browser_analyze().await {
                    Ok(state) => {
                        update(&u, state);
                        u.result
                            .set_text("Analysis refreshed. Select candidate tabs to unload.");
                    }
                    Err(e) => u.result.set_text(&format!("Browser analysis failed: {e}")),
                }
                u.busy.set(false);
                controls(&u);
            });
        });
    }
    {
        let (u, c) = (ui.clone(), client.clone());
        ui.unload.connect_clicked(move |_| {
            if u.busy.replace(true) { return; }
            controls(&u);
            let (u,c) = (u.clone(),c.clone());
            // Keep Rust's priority order when batching the explicit selection.
            let targets: Vec<_> = u.state.borrow().tabs.iter().filter(|r| u.selected.borrow().contains(&r.target))
                .take(5).map(|r|r.target.clone()).collect();
            glib::spawn_future_local(async move {
                u.result.set_text("Rechecking selected tabs and requesting unload…");
                match c.discard_tabs(&targets).await {
                    Ok(result) => {
                        u.result.set_text(&format!("{} unloaded · {} queued (not confirmed) · {} refused or no confirmation. Tabs remain open.",
                            result.confirmed.len(),result.queued.len(),result.refused.len()));
                        for target in result.confirmed.iter().chain(&result.queued) { u.selected.borrow_mut().remove(target); }
                        if let Ok(state) = c.browser_tabs().await { update(&u,state); }
                    }
                    Err(e) => u.result.set_text(&format!("Unload failed: {e}")),
                }
                u.busy.set(false); controls(&u);
            });
        });
    }
    {
        let (u, c) = (ui.clone(), client.clone());
        ui.close.connect_clicked(move |_| {
            if u.busy.get() {
                return;
            }
            let rows: Vec<_> = u
                .state
                .borrow()
                .tabs
                .iter()
                .filter(|r| r.closeable && u.selected.borrow().contains(&r.target))
                .cloned()
                .collect();
            if rows.is_empty() {
                return;
            }
            u.busy.set(true);
            controls(&u);
            let targets: Vec<_> = rows.iter().map(|r| r.target.clone()).collect();
            let titles = rows
                .iter()
                .take(8)
                .map(|r| r.title.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let dialog = gtk4::AlertDialog::builder()
                .message(format!("Close {} selected tab(s)?", targets.len()))
                .detail(format!(
                    "{titles}\n\nThis closes the tabs. Unsaved work may be lost."
                ))
                .buttons(["Cancel", "Close tabs"])
                .cancel_button(0)
                .default_button(0)
                .modal(true)
                .build();
            let root = u.close.root().and_downcast::<gtk4::Window>();
            let (u, c) = (u.clone(), c.clone());
            dialog.choose(
                root.as_ref(),
                None::<&gtk4::gio::Cancellable>,
                move |answer| {
                    if answer != Ok(1) {
                        u.busy.set(false);
                        controls(&u);
                        return;
                    }
                    glib::spawn_future_local(async move {
                        u.result.set_text("Rechecking and closing selected tabs…");
                        match c.close_tabs(&targets).await {
                            Ok(result) => {
                                u.result.set_text(&format!(
                                    "{} closed · {} queued · {} refused or unconfirmed",
                                    result.confirmed.len(),
                                    result.queued.len(),
                                    result.refused.len()
                                ));
                                for t in result.confirmed.iter().chain(&result.queued) {
                                    u.selected.borrow_mut().remove(t);
                                }
                                if let Ok(state) = c.browser_tabs().await {
                                    update(&u, state);
                                }
                            }
                            Err(e) => u.result.set_text(&format!("Close failed: {e}")),
                        }
                        u.busy.set(false);
                        controls(&u);
                    });
                },
            );
        });
    }
    {
        let u = ui.clone();
        ui.search.connect_search_changed(move |_| render(&u));
    }
    if poll {
        let ui = ui.clone();
        let weak = outer.downgrade();
        glib::spawn_future_local(async move {
            loop {
                if weak.upgrade().is_none() {
                    break;
                }
                if !ui.busy.get() {
                    match client.browser_tabs().await {
                        Ok(state) => update(&ui,state),
                        Err(e) => ui.summary.set_text(&format!("Browser data unavailable: {e}. Update/restart the Rust daemon if this endpoint is missing.")),
                    }
                }
                glib::timeout_future(Duration::from_secs(10)).await;
            }
        });
    }
    (outer, ui)
}

fn controls(ui: &Rc<Ui>) {
    ui.refresh.set_sensitive(!ui.busy.get());
    let state = ui.state.borrow();
    let selected = ui.selected.borrow();
    let can_unload = !selected.is_empty()
        && selected
            .iter()
            .all(|t| state.tabs.iter().any(|r| &r.target == t && r.eligible));
    let can_close = !selected.is_empty()
        && selected
            .iter()
            .all(|t| state.tabs.iter().any(|r| &r.target == t && r.closeable));
    ui.unload.set_sensitive(!ui.busy.get() && can_unload);
    ui.close.set_sensitive(!ui.busy.get() && can_close);
    ui.list.set_sensitive(!ui.busy.get());
    let state = ui.state.borrow();
    if state.browsers_connected > 0 {
        ui.summary.set_text(&format!(
            "{} tabs · {} selectable · {} selected",
            state.tabs.len(),
            state.tabs.iter().filter(|r| r.eligible || r.closeable).count(),
            ui.selected.borrow().len()
        ));
        ui.summary.set_tooltip_text(Some(&format!(
            "{} browser(s) · {} tabs already unloaded",
            state.browsers_connected,
            state.tabs.iter().filter(|r| r.status == "unloaded").count()
        )));
    }
}
fn update(ui: &Rc<Ui>, state: BrowserState) {
    ui.selected.borrow_mut().retain(|target| {
        state
            .tabs
            .iter()
            .any(|r| (r.eligible || r.closeable) && &r.target == target)
    });
    *ui.state.borrow_mut() = state;
    render(ui);
}
fn render(ui: &Rc<Ui>) {
    let state = ui.state.borrow();
    if state.browsers_connected == 0 {
        ui.summary
            .set_text("No browser connected. Enable the extension, then refresh.");
    }
    let y = ui.scroll.vadjustment().value();
    let filter = ui.search.text().to_lowercase();
    ui.syncing.set(true);
    let mut widgets = ui.widgets.borrow_mut();
    widgets.retain(|target, row| {
        let keep = state.tabs.iter().any(|t| &t.target == target);
        if !keep {
            ui.list.remove(&row.line);
        }
        keep
    });
    let mut previous: Option<gtk4::Box> = None;
    for row in &state.tabs {
        let view = widgets.entry(row.target.clone()).or_insert_with(|| {
            let line = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
            let check = gtk4::CheckButton::new();
            let title = gtk4::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk4::pango::EllipsizeMode::End)
                .max_width_chars(1)
                .build();
            let detail = gtk4::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk4::pango::EllipsizeMode::End)
                .max_width_chars(1)
                .build();
            detail.add_css_class("dim-label");
            let text = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            text.set_hexpand(true);
            text.append(&title);
            text.append(&detail);
            line.append(&check);
            line.append(&text);
            let weak = Rc::downgrade(ui);
            let target = row.target.clone();
            check.connect_toggled(move |check| {
                let Some(u) = weak.upgrade() else {
                    return;
                };
                if u.syncing.get() {
                    return;
                }
                if check.is_active() {
                    u.selected.borrow_mut().insert(target.clone());
                } else {
                    u.selected.borrow_mut().remove(&target);
                }
                controls(&u);
            });
            ui.list.append(&line);
            TabWidgets {
                line,
                check,
                title,
                detail,
            }
        });
        view.check.set_visible(row.eligible || row.closeable);
        view.check
            .set_active(ui.selected.borrow().contains(&row.target));
        view.title.set_text(if row.title.is_empty() {
            &row.target.url
        } else {
            &row.title
        });
        view.detail.set_text(&format!(
            "{} min idle · {} · {}",
            row.inactive_minutes, row.status, row.reason
        ));
        view.line.set_tooltip_text(Some(&format!(
            "{}\n{}\nBrowser {} · tab {}\n{}",
            row.title, row.target.url, row.target.browser, row.target.id, row.reason
        )));
        view.line.set_visible(
            format!(
                "{} {} {} {}",
                row.title, row.target.url, row.target.browser, row.reason
            )
            .to_lowercase()
            .contains(&filter),
        );
        ui.list.reorder_child_after(&view.line, previous.as_ref());
        previous = Some(view.line.clone());
    }
    drop(widgets);
    ui.syncing.set(false);
    controls(ui);
    let adjustment = ui.scroll.vadjustment();
    glib::idle_add_local_once(move || {
        adjustment.set_value(y.min((adjustment.upper() - adjustment.page_size()).max(0.0)))
    });
}

#[cfg(test)]
pub(crate) async fn test_refresh_stability() {
    let (pane, ui) = build(Client::new("127.0.0.1", 1).unwrap(), false);
    let window = gtk4::Window::builder()
        .default_width(420)
        .child(&pane)
        .build();
    let fixture = || {
        serde_json::from_value::<BrowserState>(serde_json::json!({
        "browsers_connected":1,"candidates":40,"tabs":(0..40).map(|id|serde_json::json!({
            "browser":"fixture","id":id,"url":format!("https://example.org/{id}"),
            "title":format!("Long browser tab {id} with a title that must not widen the window"),
            "inactive_minutes":120,"eligible":true,"status":"candidate","reason":"idle background tab","priority":120
        })).collect::<Vec<_>>()
    })).unwrap()
    };
    update(&ui, fixture());
    window.present();
    glib::timeout_future(Duration::from_millis(100)).await;
    assert!(
        window.width() <= 440,
        "browser forces width {}",
        window.width()
    );
    assert_eq!(ui.scroll.height(), 120);
    let target = ui.state.borrow().tabs[10].target.clone();
    let check = ui.widgets.borrow()[&target].check.clone();
    check.set_active(true);
    assert!(ui.selected.borrow().contains(&target));
    assert!(ui.unload.is_sensitive());
    ui.scroll.vadjustment().set_value(250.0);
    let position = ui.scroll.vadjustment().value();
    assert!(position > 0.0);
    let mut state = fixture();
    state.tabs[10].inactive_minutes = 121;
    update(&ui, state);
    glib::timeout_future(Duration::from_millis(100)).await;
    assert_eq!(
        check,
        ui.widgets.borrow()[&target].check,
        "refresh replaced checkbox"
    );
    assert!(check.is_active());
    assert!(
        (ui.scroll.vadjustment().value() - position).abs() < 1.0,
        "refresh moved scroll position"
    );
    check.set_active(false);
    assert!(!ui.unload.is_sensitive());
    check.set_active(true);
    let mut state = fixture();
    state.tabs[10].eligible = false;
    state.tabs[10].closeable = false;
    state.tabs[10].status = "protected".into();
    update(&ui, state);
    assert!(!check.is_visible());
    assert!(!ui.selected.borrow().contains(&target));
    let mut state = fixture();
    state.tabs[10].eligible = false;
    state.tabs[10].closeable = true;
    state.tabs[10].status = "update needed".into();
    update(&ui, state);
    assert!(check.is_visible());
    check.set_active(true);
    assert!(ui.close.is_sensitive());
    assert!(!ui.unload.is_sensitive());
    let snapshot = ui.state.borrow().clone();
    update(&ui, snapshot);
    assert!(check.is_active());
    ui.close.emit_clicked();
    glib::timeout_future(Duration::from_millis(100)).await;
    assert!(ui.busy.get(), "close should wait for confirmation");
    fn cancel_button(w: &gtk4::Widget) -> Option<gtk4::Button> {
        if let Ok(button) = w.clone().downcast::<gtk4::Button>()
            && button.is_mapped()
            && button.label().as_deref() == Some("Cancel")
        {
            return Some(button);
        }
        let mut child = w.first_child();
        while let Some(w) = child {
            if let Some(button) = cancel_button(&w) {
                return Some(button);
            }
            child = w.next_sibling();
        }
        None
    }
    let cancel = gtk4::Window::list_toplevels()
        .iter()
        .find_map(cancel_button)
        .expect("close confirmation has Cancel");
    cancel.emit_clicked();
    glib::timeout_future(Duration::from_millis(100)).await;
    assert!(!ui.busy.get());
    assert!(check.is_active());
    assert_eq!(ui.result.text(), "", "cancel must not attempt closing");
    window.close();
}
