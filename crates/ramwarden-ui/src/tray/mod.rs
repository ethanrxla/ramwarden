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
pub mod poller;
pub mod views;

use std::process::Child;
use std::time::{Duration, Instant};

use eframe::egui;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
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
    tab: Tab,
    view: UiState,
}

impl eframe::App for WindowApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let snap = self.poller.snapshot();

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Overview, "Overview");
                ui.selectable_value(&mut self.tab, Tab::Processes, "Processes");
                ui.selectable_value(&mut self.tab, Tab::Debloat, "Privacy & startup");
                ui.selectable_value(&mut self.tab, Tab::Gpu, "GPU");
                if snap.busy {
                    ui.spinner();
                }
            });
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
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Overview => views::overview(ui, &snap),
            Tab::Processes => command = views::processes(ui, &snap, &mut self.view),
            Tab::Debloat => command = views::debloat(ui, &snap, &mut self.view),
            Tab::Gpu => command = views::gpu(ui, &snap),
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
pub fn run_window(client: Client) -> Result<(), String> {
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
            let ctx = cc.egui_ctx.clone();
            Ok(Box::new(WindowApp {
                poller: Poller::start(client, true, move || ctx.request_repaint()),
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
    open: MenuId,
    quit: MenuId,
    shown_fraction: f32,
}

impl Tray {
    fn new() -> Result<Self, String> {
        let open = MenuItem::new("Open RamWarden", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::new();
        menu.append(&open).map_err(|e| e.to_string())?;
        menu.append(&quit).map_err(|e| e.to_string())?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("RamWarden")
            .with_icon(gauge_icon(0.0).ok_or("icon")?)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Tray {
            icon,
            open: open.id().clone(),
            quit: quit.id().clone(),
            shown_fraction: -1.0,
        })
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
        if (f - self.shown_fraction).abs() >= 0.01 {
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
fn open_window(child: &mut Option<Child>) {
    if let Some(c) = child
        && matches!(c.try_wait(), Ok(None))
    {
        return; // already open
    }
    match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).arg("--window").spawn()) {
        Ok(c) => *child = Some(c),
        Err(e) => tracing::error!("cannot open the window: {e}"),
    }
}

/// Run the tray until Quit. `start_hidden` (the logon task) does not open the
/// window at start.
pub fn run_tray(client: Client, start_hidden: bool) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage};

    let poller = Poller::start(client, false, || {});
    let mut tray = Tray::new()?;
    let mut window: Option<Child> = None;
    if !start_hidden {
        open_window(&mut window);
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
                open_window(&mut window);
            }
        }
        while let Ok(e) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click { button: MouseButton::Left, .. } = e {
                open_window(&mut window);
            }
        }
        if last.elapsed() >= Duration::from_secs(2) {
            tray.update(&poller);
            last = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
