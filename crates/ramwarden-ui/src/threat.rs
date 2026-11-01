//! The ThreatWarden pane in the GTK window.
//!
//! A thin view over [`ThreatPane`]: what it says and which button it offers
//! are decided in `crate::wardens`, shared with the Windows window. This file
//! only lays it out in the same style as the RAM pane beside it (dim detail
//! lines, the level colours of the process legend, one suggested action).
//!
//! The pane is rebuilt only when what it shows changes, so a refresh never
//! resets the scroll position under the user.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;

use crate::client::Client;
use crate::wardens::{PaneAction, Sprite, ThreatPane};

/// ThreatWarden's status changes on the daemon's poll (every minute or so);
/// this is quick enough for a "Turn on" to show its progress.
const REFRESH: Duration = Duration::from_secs(8);

/// The pane and the level dot on its switcher button.
pub struct Pane {
    pub widget: gtk4::Box,
    /// Kept in step with the pane's level, for the switcher.
    pub badge: gtk4::Label,
}

struct Ui {
    icon: gtk4::Picture,
    content: gtk4::Box,
    badge: gtk4::Label,
    shown: RefCell<Option<ThreatPane>>,
    icon_loaded: RefCell<bool>,
}

pub fn pane(client: Rc<Client>) -> Pane {
    let widget = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    widget.set_widget_name("threatwarden");
    widget.set_margin_top(12);
    widget.set_margin_bottom(12);
    widget.set_margin_start(14);
    widget.set_margin_end(14);

    let ui = Rc::new(Ui {
        icon: gtk4::Picture::builder()
            .width_request(48)
            .height_request(48)
            .can_shrink(false)
            .build(),
        content: gtk4::Box::new(gtk4::Orientation::Vertical, 6),
        badge: gtk4::Label::new(None),
        shown: RefCell::new(None),
        icon_loaded: RefCell::new(false),
    });
    let scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .child(&ui.content)
        .build();
    widget.append(&scroll);
    ui.content.append(&label("Loading ThreatWarden…", false));

    let (c, u) = (client, ui.clone());
    glib::spawn_future_local(async move {
        loop {
            let p = c.threat_pane().await;
            if !*u.icon_loaded.borrow()
                && let Some(path) = p.icon.clone()
                && let Ok(sprite) = c.warden_icon(&path).await
            {
                u.icon.set_paintable(Some(&texture(&sprite)));
                *u.icon_loaded.borrow_mut() = true;
            }
            render(&c, &u, p);
            glib::timeout_future(REFRESH).await;
        }
    });

    Pane {
        widget,
        badge: ui.badge.clone(),
    }
}

fn texture(s: &Sprite) -> gtk4::gdk::MemoryTexture {
    gtk4::gdk::MemoryTexture::new(
        s.width as i32,
        s.height as i32,
        gtk4::gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from(&s.rgba),
        (s.width * 4) as usize,
    )
}

fn esc(s: &str) -> glib::GString {
    glib::markup_escape_text(s)
}

fn label(text: &str, dim: bool) -> gtk4::Label {
    let l = gtk4::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk4::pango::WrapMode::WordChar)
        .max_width_chars(1)
        .hexpand(true)
        .build();
    if dim {
        l.add_css_class("dim-label");
    }
    l
}

fn markup(m: &str) -> gtk4::Label {
    let l = label("", false);
    l.set_markup(m);
    l
}

fn heading(text: &str) -> gtk4::Label {
    markup(&format!("<b>{}</b>", esc(text)))
}

fn render(client: &Rc<Client>, ui: &Rc<Ui>, p: ThreatPane) {
    ui.badge
        .set_markup(&format!("<span foreground='{}'>●</span>", p.level.colour()));
    ui.badge.set_tooltip_text(Some(p.level.label()));
    if ui.shown.borrow().as_ref() == Some(&p) {
        return;
    }
    while let Some(child) = ui.content.first_child() {
        ui.content.remove(&child);
    }
    let c = &ui.content;

    // ── who, and how it is ──────────────────────────────────────────────────
    let head = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    // The picture is kept across rebuilds so the icon is fetched once.
    if let Some(old) = ui.icon.parent().and_downcast::<gtk4::Box>() {
        old.remove(&ui.icon);
    }
    head.append(&ui.icon);
    let title = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    title.set_valign(gtk4::Align::Center);
    title.append(&markup(&format!(
        "<b>{}</b>  <span foreground='{}'>● {}</span>",
        esc(&p.name),
        p.level.colour(),
        p.level.label()
    )));
    if !p.headline.is_empty() {
        title.append(&label(&p.headline, false));
    }
    head.append(&title);
    c.append(&head);
    if !p.detail.is_empty() {
        c.append(&label(&p.detail, true));
    }
    if let Some(s) = &p.summary {
        c.append(&markup(&format!("<i>{}</i>", esc(s))));
    }

    // ── the one thing the user can do ───────────────────────────────────────
    match &p.action {
        PaneAction::Upgrade { url, price } => {
            let b = gtk4::Button::with_label(&format!("Get a ThreatWarden pass · {price}"));
            b.add_css_class("suggested-action");
            b.set_tooltip_text(Some(url));
            let url = url.clone();
            b.connect_clicked(move |b| {
                let win = b.root().and_downcast::<gtk4::Window>();
                gtk4::UriLauncher::new(&url).launch(
                    win.as_ref(),
                    None::<&gtk4::gio::Cancellable>,
                    |r| {
                        if let Err(e) = r {
                            tracing::warn!("could not open the pricing page: {e}");
                        }
                    },
                );
            });
            c.append(&b);
            c.append(&label(
                "Pay with Phantom, MetaMask or card. Your RAM warden stays free.",
                true,
            ));
        }
        PaneAction::TurnOn => {
            let b = gtk4::Button::with_label("Turn on");
            b.add_css_class("suggested-action");
            b.set_tooltip_text(Some("Installs Wazuh's own agent and connects it. Your system asks for your password once."));
            let note = label("", true);
            let (cl, u, n) = (client.clone(), ui.clone(), note.clone());
            b.connect_clicked(move |b| {
                b.set_sensitive(false);
                let (cl, u, n, b) = (cl.clone(), u.clone(), n.clone(), b.clone());
                glib::spawn_future_local(async move {
                    n.set_text("Starting setup…");
                    match cl.warden_setup(crate::wardens::THREAT).await {
                        Ok(r) if r.started => {
                            n.set_text("Setting up. Approve the system prompt when it appears.")
                        }
                        Ok(r) => {
                            n.set_text(&r.error.unwrap_or_else(|| "Setup did not start.".into()))
                        }
                        Err(e) => n.set_text(&format!("Setup did not start: {e}")),
                    }
                    b.set_sensitive(true);
                    let p = cl.threat_pane().await;
                    render(&cl, &u, p);
                });
            });
            c.append(&b);
            c.append(&note);
        }
        PaneAction::None => {}
    }

    // ── engines ─────────────────────────────────────────────────────────────
    if !p.engines.is_empty() {
        c.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        c.append(&heading(if p.action.is_upgrade() {
            "What the pass turns on"
        } else {
            "Engines"
        }));
        for e in &p.engines {
            let detail = if e.detail.is_empty() {
                String::new()
            } else {
                format!(" — {}", esc(&e.detail))
            };
            c.append(&markup(&format!(
                "<span foreground='{}'>●</span> {}<span alpha='70%'>{detail}</span>",
                e.colour(),
                esc(&e.name)
            )));
        }
    }

    // ── alerts ──────────────────────────────────────────────────────────────
    if !p.action.is_upgrade() {
        c.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        c.append(&heading("Recent alerts"));
        if p.alerts.is_empty() {
            c.append(&label("Nothing at or above your alert level.", true));
        }
        for a in &p.alerts {
            let card = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
            card.set_margin_bottom(4);
            card.append(&markup(&format!(
                "<span foreground='{}'><b>{}</b></span>  <small>{} · {}</small>",
                a.colour,
                esc(&a.tag),
                esc(a.engine),
                esc(&a.when)
            )));
            card.append(&label(&a.title, false));
            if !a.explanation.is_empty() {
                card.append(&label(&a.explanation, true));
            }
            if !a.advice.is_empty() {
                card.append(&markup(&format!("<i>{}</i>", esc(&a.advice))));
            }
            c.append(&card);
        }
    }

    if !p.agents.is_empty() {
        c.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        c.append(&heading("Your other computers"));
        for a in &p.agents {
            c.append(&label(a, true));
        }
    }
    for problem in &p.problems {
        c.append(&markup(&format!(
            "<span foreground='#facc15'>⚠</span> {}",
            esc(problem)
        )));
    }
    c.append(&label(
        "ThreatWarden only watches and explains. It never deletes, quarantines or blocks anything on its own.",
        true,
    ));
    *ui.shown.borrow_mut() = Some(p);
}
