//! The warden's speech bubble in the GTK window (Linux).
//!
//! A ram and a rounded bubble across the top of the window, laid out like the
//! Mac's. It polls `/warden/bubble`, shows the choices of a check-in as
//! buttons, and posts the answer back. Under it is a text box that is always
//! there: Enter answers the focus question, or talks to the warden when it
//! has not asked anything. What to say and when is the daemon's call; see
//! [`crate::voice`] and `docs/specs/warden-voice.md`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk4::{glib, prelude::*};

use crate::client::Client;
use crate::voice::{Bubble, FocusProposal, MUTE_CHOICES, Said, Speech, TALK_PLACEHOLDER};

const POLL: Duration = Duration::from_secs(5);

const CSS: &str = "
.warden-bubble { border: 2px solid #c0392b; border-radius: 14px; padding: 8px 12px; }
.warden-ram { font-size: 22px; }
.warden-you { opacity: 0.7; font-size: smaller; }
";

struct Ui {
    root: gtk4::Box,
    text: gtk4::Label,
    choices: gtk4::Box,
    mute: gtk4::MenuButton,
    unmute: gtk4::Button,
    close: gtk4::Button,
    shown: RefCell<Option<Bubble>>,
    /// The always-visible text box, and the warden's answer to it.
    entry: gtk4::Entry,
    said: gtk4::Box,
    /// The last thing typed was a focus: the reply stays until put away.
    talking: RefCell<bool>,
}

/// The bubble bar. Hidden while the warden has nothing to say.
pub fn bar(client: Client) -> gtk4::Box {
    if let Some(display) = gtk4::gdk::Display::default() {
        let css = gtk4::CssProvider::new();
        css.load_from_data(CSS);
        gtk4::style_context_add_provider_for_display(
            &display,
            &css,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
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
    let text = gtk4::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .selectable(true)
        .build();
    let mute = gtk4::MenuButton::builder()
        .icon_name("audio-volume-high-symbolic")
        .tooltip_text("Quiet, please")
        .build();
    let unmute = gtk4::Button::with_label("Unmute");
    let close = gtk4::Button::from_icon_name("window-close-symbolic");
    close.set_tooltip_text(Some("Dismiss"));
    for b in [
        mute.upcast_ref::<gtk4::Widget>(),
        unmute.upcast_ref(),
        close.upcast_ref(),
    ] {
        b.add_css_class("flat");
        b.set_valign(gtk4::Align::Start);
    }
    head.append(&text);
    head.append(&unmute);
    head.append(&mute);
    head.append(&close);
    let choices = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    let said = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    said.set_visible(false);
    let entry = gtk4::Entry::builder()
        .placeholder_text(TALK_PLACEHOLDER)
        .secondary_icon_name("mail-send-symbolic")
        .secondary_icon_tooltip_text("Send (Enter)")
        .hexpand(true)
        .build();
    bubble.append(&head);
    bubble.append(&choices);
    bubble.append(&said);
    bubble.append(&entry);
    root.append(&ram);
    root.append(&bubble);
    root.set_visible(false);

    let ui = Rc::new(Ui {
        root: root.clone(),
        text,
        choices,
        mute,
        unmute,
        close,
        shown: RefCell::new(None),
        entry: entry.clone(),
        said,
        talking: RefCell::new(false),
    });
    {
        let (c, u) = (client.clone(), ui.clone());
        entry.connect_activate(move |_| send(&c, &u));
        let (c, u) = (client.clone(), ui.clone());
        entry.connect_icon_release(move |_, _| send(&c, &u));
    }
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
            let Some(b) = u.shown.borrow().clone() else {
                return;
            };
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
    ui.text.set_visible(!text.is_empty());
    ui.root.set_visible(true);
}

/// Send what was typed: a focus when the warden asked for one, otherwise a
/// chat message. The reply shows under the bubble.
fn send(client: &Rc<Client>, ui: &Rc<Ui>) {
    let text = ui.entry.text().trim().to_string();
    if text.is_empty() || !ui.entry.is_sensitive() {
        return;
    }
    let focus = ui.shown.borrow().as_ref().is_some_and(|b| b.asks_focus());
    if let Some(why) = crate::voice::too_long(&text, focus) {
        show_said(
            client,
            ui,
            Said {
                you: text,
                reply: why.into(),
                ..Said::default()
            },
        );
        return;
    }
    ui.entry.set_text("");
    ui.entry.set_sensitive(false);
    ui.entry.set_placeholder_text(Some("Thinking…"));
    let (c, u) = (client.clone(), ui.clone());
    glib::spawn_future_local(async move {
        let said = c.say_to_warden(&text, focus).await;
        u.entry.set_sensitive(true);
        u.entry.grab_focus();
        show_said(&c, &u, said);
        refresh(&c, &u).await;
    });
}

/// What you typed and the warden's answer, with the focus checklist or the
/// proposed change under it.
fn show_said(client: &Rc<Client>, ui: &Rc<Ui>, said: Said) {
    while let Some(child) = ui.said.first_child() {
        ui.said.remove(&child);
    }
    *ui.talking.borrow_mut() = true;
    let head = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    let you = gtk4::Label::builder()
        .label(format!("You: {}", said.you))
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .build();
    you.add_css_class("warden-you");
    let away = gtk4::Button::from_icon_name("window-close-symbolic");
    away.add_css_class("flat");
    away.set_tooltip_text(Some("Put this away"));
    {
        let u = ui.clone();
        away.connect_clicked(move |_| hide_said(&u));
    }
    head.append(&you);
    head.append(&away);
    ui.said.append(&head);
    let reply = gtk4::Label::builder()
        .label(&said.reply)
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    // Long replies scroll inside the bubble instead of growing the window.
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    body.append(&reply);
    if let Some(list) = said.focus.clone() {
        checklist(client, ui, &body, list);
    }
    let scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(180)
        .child(&body)
        .build();
    ui.said.append(&scroll);
    if let Some(p) = said.proposed {
        let review = gtk4::Button::with_label("Review fix…");
        review.set_tooltip_text(Some(&p.label));
        review.set_halign(gtk4::Align::Start);
        let (c, u) = (client.clone(), ui.clone());
        review.connect_clicked(move |b| confirm(&c, &u, b, p.clone()));
        ui.said.append(&review);
    }
    ui.said.set_visible(true);
    ui.root.set_visible(true);
}

fn hide_said(ui: &Ui) {
    *ui.talking.borrow_mut() = false;
    ui.said.set_visible(false);
}

/// The tabs the warden thinks belong to your focus, as checkboxes.
fn checklist(client: &Rc<Client>, ui: &Rc<Ui>, body: &gtk4::Box, list: FocusProposal) {
    let title = gtk4::Label::builder()
        .label("Relevant to this task")
        .xalign(0.0)
        .build();
    title.add_css_class("heading");
    body.append(&title);
    let ticks: Vec<gtk4::CheckButton> = list
        .tabs
        .iter()
        .map(|t| {
            let b = gtk4::CheckButton::with_label(if t.title.is_empty() {
                "Web page"
            } else {
                &t.title
            });
            b.set_active(t.selected);
            b.set_tooltip_text(Some(&t.url));
            body.append(&b);
            b
        })
        .collect();
    let keep = gtk4::Button::with_label("Use these preferences");
    keep.set_halign(gtk4::Align::Start);
    body.append(&keep);
    let (c, u) = (client.clone(), ui.clone());
    keep.connect_clicked(move |b| {
        b.set_sensitive(false);
        let mut tabs = list.tabs.clone();
        for (tab, tick) in tabs.iter_mut().zip(&ticks) {
            tab.selected = tick.is_active();
        }
        let (c, u, token) = (c.clone(), u.clone(), list.token.clone());
        glib::spawn_future_local(async move {
            let text = match c.focus_feedback(&token, &tabs).await {
                Ok(_) => format!(
                    "Preferences updated · {} tabs marked relevant",
                    tabs.iter().filter(|t| t.selected).count()
                ),
                Err(e) => {
                    format!("That tab list expired or changed. Tell me your focus again. ({e})")
                }
            };
            hide_said(&u);
            say(&u, &text);
        });
    });
}

/// Apply a proposed change only after the user confirms it in a dialog.
fn confirm(client: &Rc<Client>, ui: &Rc<Ui>, button: &gtk4::Button, p: crate::voice::Proposed) {
    let dialog = gtk4::AlertDialog::builder()
        .message(&p.label)
        .detail("Apply this change? Nothing else will be changed.")
        .buttons(["Cancel", "Apply fix"])
        .cancel_button(0)
        .default_button(0)
        .build();
    let window = button.root().and_downcast::<gtk4::Window>();
    let (c, u, b) = (client.clone(), ui.clone(), button.clone());
    dialog.choose(window.as_ref(), gtk4::gio::Cancellable::NONE, move |r| {
        if r != Ok(1) {
            return;
        }
        b.set_sensitive(false);
        let (c, u) = (c.clone(), u.clone());
        glib::spawn_future_local(async move {
            let text = match c.chat_action(&p.token).await {
                Ok(r) if r.reply.is_empty() => "The action finished without a report.".to_string(),
                Ok(r) => r.reply,
                Err(e) => format!(
                    "That proposal expired or the computer changed; nothing was applied. ({e})"
                ),
            };
            hide_said(&u);
            say(&u, &text);
        });
    });
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
        ui.entry.set_placeholder_text(Some(TALK_PLACEHOLDER));
        ui.close.set_visible(false);
        if speech.muted {
            say(ui, "Your warden is keeping quiet.");
        } else if !*ui.talking.borrow() {
            say(ui, "All quiet.");
        }
        return;
    };
    ui.close.set_visible(true);
    if ui
        .shown
        .borrow()
        .as_ref()
        .is_some_and(|b| b.id == bubble.id)
    {
        return; // already showing it; keep the buttons the user may be reaching for
    }
    say(ui, &bubble.text);
    ui.entry.set_placeholder_text(Some(bubble.placeholder()));
    for choice in bubble.buttons() {
        let b = gtk4::Button::with_label(&choice.label);
        if choice.id == "close" {
            b.add_css_class("destructive-action");
        } else if choice.id == "keep" {
            b.add_css_class("suggested-action");
        }
        let (c, u, id, pick) = (
            client.clone(),
            ui.clone(),
            bubble.id.clone(),
            choice_id(&choice.id),
        );
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
