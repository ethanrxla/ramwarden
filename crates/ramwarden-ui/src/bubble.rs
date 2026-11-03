//! The warden's speech bubble in the GTK window (Linux).
//!
//! A ram and a rounded bubble across the top of the window. It polls
//! `/warden/bubble`, shows the choices of a check-in as buttons, and posts the
//! answer back. What to say and when is the daemon's call; see
//! [`crate::voice`] and `docs/specs/warden-voice.md`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk4::{glib, prelude::*};

use crate::client::Client;
use crate::voice::{Bubble, MUTE_CHOICES, Speech};

const POLL: Duration = Duration::from_secs(5);

const CSS: &str = "
.warden-bubble { border: 2px solid #c0392b; border-radius: 14px; padding: 8px 12px; }
.warden-ram { font-size: 22px; }
";

struct Ui {
    root: gtk4::Box,
    text: gtk4::Label,
    choices: gtk4::Box,
    mute: gtk4::MenuButton,
    unmute: gtk4::Button,
    close: gtk4::Button,
    shown: RefCell<Option<Bubble>>,
}

/// The bubble bar. Hidden while the warden has nothing to say.
pub fn bar(client: Client) -> gtk4::Box {
    if let Some(display) = gtk4::gdk::Display::default() {
        let css = gtk4::CssProvider::new();
        css.load_from_data(CSS);
        gtk4::style_context_add_provider_for_display(&display, &css, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
    let client = Rc::new(client);
    let root = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let ram = gtk4::Label::new(Some("🐏"));
    ram.add_css_class("warden-ram");
    ram.set_valign(gtk4::Align::Start);
    ram.set_tooltip_text(Some("Your warden"));
    let bubble = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    bubble.add_css_class("warden-bubble");
    bubble.set_hexpand(true);
    let head = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    let text = gtk4::Label::builder().xalign(0.0).wrap(true).hexpand(true).selectable(true).build();
    let mute = gtk4::MenuButton::builder().icon_name("audio-volume-muted-symbolic").tooltip_text("Quiet, please").build();
    let unmute = gtk4::Button::with_label("Unmute");
    let close = gtk4::Button::from_icon_name("window-close-symbolic");
    close.set_tooltip_text(Some("Dismiss"));
    for b in [mute.upcast_ref::<gtk4::Widget>(), unmute.upcast_ref(), close.upcast_ref()] {
        b.add_css_class("flat");
        b.set_valign(gtk4::Align::Start);
    }
    head.append(&text);
    head.append(&unmute);
    head.append(&mute);
    head.append(&close);
    let choices = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    bubble.append(&head);
    bubble.append(&choices);
    root.append(&ram);
    root.append(&bubble);
    root.set_visible(false);

    let ui = Rc::new(Ui { root: root.clone(), text, choices, mute, unmute, close, shown: RefCell::new(None) });
    ui.mute.set_popover(Some(&mute_menu(&client, &ui)));
    {
        let (c, u) = (client.clone(), ui.clone());
        ui.unmute.connect_clicked(move |_| {
            let (c, u) = (c.clone(), u.clone());
            glib::spawn_future_local(async move {
                let _ = c.mute(false, None).await;
                refresh(&c, &u).await;
            });
        });
    }
    {
        let (c, u) = (client.clone(), ui.clone());
        ui.close.connect_clicked(move |_| {
            let Some(b) = u.shown.borrow().clone() else { return };
            answer(&c, &u, b.id, "dismiss");
        });
    }
    {
        let (c, u) = (client.clone(), ui.clone());
        glib::spawn_future_local(async move {
            loop {
                refresh(&c, &u).await;
                glib::timeout_future(POLL).await;
            }
        });
    }
    root
}

fn mute_menu(client: &Rc<Client>, ui: &Rc<Ui>) -> gtk4::Popover {
    let list = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    let pop = gtk4::Popover::new();
    for (label, hours) in MUTE_CHOICES {
        let b = gtk4::Button::with_label(label);
        b.add_css_class("flat");
        let (c, u, p) = (client.clone(), ui.clone(), pop.clone());
        b.connect_clicked(move |_| {
            p.popdown();
            let (c, u) = (c.clone(), u.clone());
            glib::spawn_future_local(async move {
                if let Ok(r) = c.mute(true, hours).await {
                    say(&u, &r.reply);
                }
                glib::timeout_future(Duration::from_secs(3)).await;
                refresh(&c, &u).await;
            });
        });
        list.append(&b);
    }
    pop.set_child(Some(&list));
    pop
}

/// Show a short line with no buttons, until the next poll replaces it.
fn say(ui: &Ui, text: &str) {
    while let Some(child) = ui.choices.first_child() {
        ui.choices.remove(&child);
    }
    ui.text.set_text(text);
    ui.root.set_visible(!text.is_empty());
}

fn answer(client: &Rc<Client>, ui: &Rc<Ui>, id: String, choice: &'static str) {
    ui.choices.set_sensitive(false);
    let (c, u) = (client.clone(), ui.clone());
    glib::spawn_future_local(async move {
        let reply = c.answer(&id, choice).await;
        u.choices.set_sensitive(true);
        *u.shown.borrow_mut() = None;
        match reply {
            Ok(r) => say(&u, &r.reply),
            Err(e) => say(&u, &format!("I couldn't reach the daemon: {e}")),
        }
    });
}

async fn refresh(client: &Rc<Client>, ui: &Rc<Ui>) {
    match client.bubble().await {
        Ok(speech) => render(client, ui, speech),
        // A daemon that is restarting just means a quiet warden.
        Err(_) => ui.root.set_visible(false),
    }
}

fn render(client: &Rc<Client>, ui: &Rc<Ui>, speech: Speech) {
    if !speech.pet {
        *ui.shown.borrow_mut() = None;
        ui.root.set_visible(false);
        return;
    }
    ui.unmute.set_visible(speech.muted);
    ui.mute.set_visible(!speech.muted);
    let Some(bubble) = speech.bubble else {
        *ui.shown.borrow_mut() = None;
        if speech.muted {
            say(ui, "Your warden is keeping quiet.");
            ui.close.set_visible(false);
        } else {
            say(ui, "");
        }
        return;
    };
    ui.close.set_visible(true);
    if ui.shown.borrow().as_ref().is_some_and(|b| b.id == bubble.id) {
        return; // already showing it; keep the buttons the user may be reaching for
    }
    say(ui, &bubble.text);
    for choice in &bubble.choices {
        let b = gtk4::Button::with_label(&choice.label);
        if choice.id == "close" {
            b.add_css_class("destructive-action");
        } else if choice.id == "keep" {
            b.add_css_class("suggested-action");
        }
        let (c, u, id, pick) = (client.clone(), ui.clone(), bubble.id.clone(), choice_id(&choice.id));
        b.connect_clicked(move |_| answer(&c, &u, id.clone(), pick));
        ui.choices.append(&b);
    }
    if let Some(tab) = &bubble.tab {
        ui.text.set_tooltip_text(Some(&tab.url));
    }
    *ui.shown.borrow_mut() = Some(bubble);
}

/// The daemon's choice ids, as the static strings the answer call takes.
fn choice_id(id: &str) -> &'static str {
    match id {
        "keep" => "keep",
        "close" => "close",
        "later" => "later",
        _ => "dismiss",
    }
}
