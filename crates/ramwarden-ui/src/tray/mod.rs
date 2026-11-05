//! The Windows window and tray icon, as two processes.
//!
//! # Why two
//!
//! A memory manager's own UI must not be what fills the memory. Measured on the
//! target machine:
//!
//! | build | window open | after closing |
//! |---|---|---|
//! | eframe defaults (wgpu), hidden window | 433 MB private, 253 MB VRAM | the same |
//! | glow, window torn down on close | 93 MB | 76 MB (the GL driver stays mapped) |
//! | tray host + separate window process | — | the host alone, a few MB |
//!
//! So the **tray host** ([`run_tray`]) never loads a graphics driver: it is a
//! tray icon, a Win32 message loop and a light poll of `/state` for the gauge.
//! **Open** launches this same executable with `--window` ([`run_window`]);
//! closing that window ends its process and returns everything it used.

pub mod icon;
pub mod launch;
pub mod poller;
pub mod views;

use std::process::Child;
use std::time::{Duration, Instant};

use eframe::egui;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pane { Ram, Threat }
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::client::Client;
use poller::{Command, Poller};
use views::UiState;

// ── The window process ──────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Processes,
    Debloat,
    Gpu,
}

struct WindowApp {
    poller: Poller,
    pane: Pane,
    threat_icon: Option<egui::TextureHandle>,
    tab: Tab,
    view: UiState,
}

impl eframe::App for WindowApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let snap = self.poller.snapshot();

        if self.threat_icon.is_none() && let Some(sprite)=&snap.threat_icon {
            self.threat_icon=Some(ctx.load_texture("threatwarden",egui::ColorImage::from_rgba_unmultiplied([sprite.width as usize,sprite.height as usize],&sprite.rgba),egui::TextureOptions::NEAREST));
        }

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.pane,Pane::Ram,"RAM");
                ui.selectable_value(&mut self.pane,Pane::Threat,"ThreatWarden");
                if let Some(p)=&snap.threat {ui.colored_label(views::level_colour(p.level),p.level.label());}
            });
            if self.pane==Pane::Ram {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Overview, "Overview");
                ui.selectable_value(&mut self.tab, Tab::Processes, "Processes");
                ui.selectable_value(&mut self.tab, Tab::Debloat, "Privacy & startup");
                ui.selectable_value(&mut self.tab, Tab::Gpu, "GPU");
                if snap.busy {
                    ui.spinner();
                }
            });
            }
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            if let Some(e) = &snap.error {
                ui.colored_label(egui::Color32::from_rgb(248, 113, 113), e);
            } else if let Some(m) = &snap.message {
                ui.label(m);
            } else {
                ui.label("Connected. Closing this window leaves RamWarden running in the tray.");
            }
        });

        let mut command: Option<Command> = None;
        egui::Panel::top("warden").show(ui, |ui| {
            if let Some(c) = views::bubble(ui, &snap) {
                command = Some(c);
            }
        });
        egui::CentralPanel::default().show(ui, |ui| match (self.pane,self.tab) {
            (Pane::Threat,_) => command=views::threat(ui,&snap,self.threat_icon.as_ref()),
            (Pane::Ram,Tab::Overview) => views::overview(ui, &snap),
            (Pane::Ram,Tab::Processes) => command = views::processes(ui, &snap, &mut self.view),
            (Pane::Ram,Tab::Debloat) => command = views::debloat(ui, &snap, &mut self.view),
            (Pane::Ram,Tab::Gpu) => command = views::gpu(ui, &snap),
        });

        if let Some((pending, question)) = self.view.confirm.clone() {
            egui::Window::new("Confirm")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(&ctx, |ui| {
                    ui.label(question);
                    ui.horizontal(|ui| {
                        if ui.button("Do it").clicked() {
                            command = Some(pending.clone());
                            self.view.confirm = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.view.confirm = None;
                        }
                    });
                });
        }
        if let Some(c) = command {
            self.poller.send(c);
        }
    }
}

/// Run the window until it is closed.
pub fn run_window(client: Client, pane: Pane) -> Result<(), String> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("RamWarden")
            .with_inner_size([760.0, 620.0])
            .with_min_inner_size([480.0, 360.0])
            .with_icon(egui::IconData {
                rgba: icon::gauge(0.3),
                width: icon::SIZE,
                height: icon::SIZE,
            }),
        ..Default::default()
    };
    eframe::run_native(
        "RamWarden",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            let ctx = cc.egui_ctx.clone();
            Ok(Box::new(WindowApp {
                poller: Poller::start(client, true, move || ctx.request_repaint()),
                pane,
                threat_icon: None,
                tab: Tab::Overview,
                view: UiState::default(),
            }))
        }),
    )
    .map_err(|e| e.to_string())
}

// ── The tray host ───────────────────────────────────────────────────────────

fn gauge_icon(fraction: f32) -> Option<Icon> {
    Icon::from_rgba(icon::gauge(fraction), icon::SIZE, icon::SIZE).ok()
}

/// The tray icon and its menu.
struct Tray {
    icon: TrayIcon,
    menu: Menu,
    open: MenuId,
    threat: MenuId,
    quit: MenuId,
    shown_fraction: f32,
    /// The warden's question at the top of the menu, so it can be answered
    /// with the window closed: the line, its choices, and a separator.
    voice: Option<VoiceItems>,
}

struct VoiceItems {
    bubble: String,
    line: MenuItem,
    choices: Vec<(MenuItem, Command)>,
    gap: PredefinedMenuItem,
}

impl Tray {
    fn new() -> Result<Self, String> {
        let open = MenuItem::new("Open RamWarden", true, None);
        let threat=MenuItem::new("ThreatWarden",true,None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::new();
        menu.append(&open).map_err(|e| e.to_string())?;
        menu.append(&threat).map_err(|e| e.to_string())?;
        menu.append(&quit).map_err(|e| e.to_string())?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_tooltip("RamWarden")
            .with_icon(gauge_icon(0.0).ok_or("icon")?)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Tray {
            icon,
            menu,
            open: open.id().clone(),
            threat: threat.id().clone(),
            quit: quit.id().clone(),
            shown_fraction: -1.0,
            voice: None,
        })
    }

    /// The command behind a clicked menu item, if it is one of the warden's.
    fn voice_command(&self, id: &MenuId) -> Option<Command> {
        let v = self.voice.as_ref()?;
        v.choices.iter().find(|(item, _)| item.id() == id).map(|(_, c)| c.clone())
    }

    /// Show the warden's current question in the menu and the tooltip, or
    /// take the last one away. Remarks stay in the window: the tray is for
    /// what needs an answer.
    fn show_question(&mut self, poller: &Poller) {
        let snap = poller.snapshot();
        let question = snap.speech.as_ref()
            .filter(|s| s.pet)
            .and_then(|s| s.bubble.as_ref())
            .filter(|b| b.is_question());
        if self.voice.as_ref().map(|v| v.bubble.as_str()) == question.map(|b| b.id.as_str()) {
            return;
        }
        if let Some(old) = self.voice.take() {
            let _ = self.menu.remove(&old.line);
            for (item, _) in &old.choices {
                let _ = self.menu.remove(item);
            }
            let _ = self.menu.remove(&old.gap);
            self.shown_fraction = -1.0; // restore the gauge tooltip
        }
        let Some(b) = question else { return };
        let line = MenuItem::new(format!("🐏 {}", b.text), false, None);
        let choices: Vec<(MenuItem, Command)> = b.choices.iter()
            .map(|c| (MenuItem::new(format!("    {}", c.label), true, None),
                Command::Answer { id: b.id.clone(), choice: c.id.clone() }))
            .collect();
        let gap = PredefinedMenuItem::separator();
        let _ = self.menu.insert(&line, 0);
        for (i, (item, _)) in choices.iter().enumerate() {
            let _ = self.menu.insert(item, i + 1);
        }
        let _ = self.menu.insert(&gap, choices.len() + 1);
        let _ = self.icon.set_tooltip(Some(format!("RamWarden: {}", b.text)));
        self.voice = Some(VoiceItems { bubble: b.id.clone(), line, choices, gap });
    }

    /// Redraw the gauge when the figure visibly changes.
    fn update(&mut self, poller: &Poller) {
        let snap = poller.snapshot();
        let Some(st) = snap.state else {
            if let Some(e) = snap.error {
                let _ = self.icon.set_tooltip(Some(format!("RamWarden — {e}")));
            }
            return;
        };
        let f = (st.percent / 100.0) as f32;
        if (f - self.shown_fraction).abs() >= 0.01 && self.voice.is_none() {
            let _ = self.icon.set_icon(gauge_icon(f));
            let _ = self.icon.set_tooltip(Some(format!(
                "RamWarden — {:.0}% in use, {:.1}% pressure",
                st.percent, st.psi_some
            )));
            self.shown_fraction = f;
        }
    }
}

/// Launch the window process, or leave it alone if it is already open.
fn open_window(child: &mut Option<Child>, pane: Pane) {
    if let Some(c) = child
        && matches!(c.try_wait(), Ok(None))
    {
        return; // already open
    }
    match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).arg("--window").arg(if pane==Pane::Threat {"--threat"}else{"--ram"}).spawn()) {
        Ok(c) => *child = Some(c),
        Err(e) => tracing::error!("cannot open the window: {e}"),
    }
}

/// Run the tray until Quit. `start_hidden` (the logon task) does not open the
/// window at start.
pub fn run_tray(client: Client, start_hidden: bool) -> Result<(), String> {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage};
    use windows::core::w;

    // One tray per session. Launching the app again (Start menu, a second
    // double-click) while the tray runs just opens the window.
    // SAFETY: a named mutex held for the life of the process; never closed.
    let _instance = unsafe { CreateMutexW(None, false, w!("Local\\RamWardenTray")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        if start_hidden {
            return Ok(()); // a second logon start: the tray is already there
        }
        return run_window(client,Pane::Ram);
    }

    let poller = Poller::start(client, false, || {});
    let mut tray = Tray::new()?;
    let mut window: Option<Child> = None;
    if !start_hidden {
        open_window(&mut window,Pane::Ram);
    }

    let mut last = Instant::now() - Duration::from_secs(10);
    loop {
        // SAFETY: a standard message pump on this thread's queue; the tray
        // icon's hidden window lives on it.
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        while let Ok(e) = MenuEvent::receiver().try_recv() {
            if e.id == tray.quit {
                if let Some(mut c) = window.take() {
                    let _ = c.kill();
                }
                return Ok(());
            }
            if e.id == tray.open {
                open_window(&mut window,Pane::Ram);
            }
            if e.id==tray.threat {
                open_window(&mut window,Pane::Threat);
            }
            if let Some(c) = tray.voice_command(&e.id) {
                poller.send(c);
            }
        }
        while let Ok(e) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click { button: MouseButton::Left, .. } = e {
                open_window(&mut window,Pane::Ram);
            }
        }
        if last.elapsed() >= Duration::from_secs(2) {
            tray.update(&poller);
            tray.show_question(&poller);
            last = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
