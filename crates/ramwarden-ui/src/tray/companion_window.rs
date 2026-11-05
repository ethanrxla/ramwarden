//! Independent, attached speech boxes. Dashboard windows never consume pet requests.
use super::{
    Pane,
    activation::{Reader, Surface},
    companion_ui::{self, Chat},
    pets,
    poller::Poller,
};
use crate::{client::Client, companions::Options};
use eframe::egui;
use std::time::{Duration, Instant};
struct CompanionWindow {
    client: Client,
    pane: Pane,
    surface: Surface,
    reader: Reader,
    chat: Chat,
    options: Options,
    error: Option<String>,
    poller: Option<Poller>,
    started: Instant,
    position: [f32; 2],
    size: [f32; 2],
    above: bool,
    shown: bool,
    workspace: super::workspace_ui::WorkspaceUi,
    audio_result: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    audio_status: Option<String>,
}
impl eframe::App for CompanionWindow {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.; 4]
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Apply to chat, settings and the independent upper thought window.
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let saved = Options::load();
        let hidden = if self.pane == Pane::Threat { saved.threat.capture_hidden } else { saved.ram.capture_hidden };
        if let Some(window) = frame.winit_window()
            && let Ok(handle) = window.window_handle()
            && let RawWindowHandle::Win32(handle) = handle.as_raw()
        {
            use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::*};
            // SAFETY: winit owns this live HWND on the current UI thread.
            if unsafe { SetWindowDisplayAffinity(HWND(handle.hwnd.get() as *mut _), if hidden { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE }) }.is_err() && hidden {
                self.error = Some("Windows could not exclude this speech box from screen capture.".into());
            }
        }
        if let Some(result) = self.audio_result.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.audio_result = None;
            self.audio_status = Some(match result {
                Ok(()) => "Voice playback completed.".into(),
                Err(e) => e,
            });
        }
        ctx.request_repaint_after(Duration::from_millis(400));
        if self.poller.is_none()
            && let Some((_, surface)) = self.reader.next_pane()
        {
            self.surface = surface;
            self.options = Options::load();
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        let physical_size = frame
            .winit_window()
            .map(|w| {
                let s = w.outer_size();
                [s.width as f32, s.height as f32]
            })
            .unwrap_or(self.size);
        let (position, above) =
            pets::attachment(self.pane, physical_size, self.surface == Surface::Thought);
        self.above = above;
        if !self.shown || position != self.position {
            if let Some(window) = frame.winit_window() {
                window.set_outer_position(winit::dpi::PhysicalPosition::new(
                    position[0] as i32,
                    position[1] as i32,
                ));
            }
            self.position = position;
        }
        if !self.shown {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            self.shown = true;
        }
        let snap = self.poller.as_ref().map(Poller::snapshot);
        let bubble = snap
            .as_ref()
            .and_then(|s| s.speech.as_ref())
            .filter(|s| s.pet && !s.muted)
            .and_then(|s| s.bubble.as_ref())
            .filter(|b| matching_warden(&b.warden, self.pane));
        if self.surface == Surface::Thought
            && ((bubble.is_none() && self.started.elapsed() > Duration::from_secs(12))
                || self.started.elapsed() > Duration::from_secs(600))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                if !self.above {
                    tail(ui, false);
                }
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(19, 21, 29, 248))
                    .stroke(egui::Stroke::new(2., egui::Color32::from_gray(235)))
                    .corner_radius(16)
                    .inner_margin(12)
                    .show(ui, |ui| {
                        ui.set_min_width(self.size[0] - 32.);
                        ui.set_min_height(self.size[1] - 48.);
                        ui.horizontal(|ui| {
                            ui.strong(if self.pane == Pane::Ram {
                                "RamWarden"
                            } else {
                                "ThreatWarden"
                            });
                            if self.surface != Surface::Thought {
                                ui.selectable_value(&mut self.surface, Surface::Chat, "Chat");
                                if ui
                                    .selectable_value(
                                        &mut self.surface,
                                        Surface::Settings,
                                        "Settings",
                                    )
                                    .clicked()
                                {
                                    self.options = Options::load();
                                }
                            }
                        });
                        ui.separator();
                        if self.surface == Surface::Thought {
                            if let Some(snap) = &snap {
                                if bubble.is_some() {
                                    if let Some(command) = super::views::bubble(ui, snap)
                                        && let Some(poller) = &self.poller
                                    {
                                        poller.send(command);
                                    }
                                } else {
                                    ui.label("Listening for a thought…");
                                }
                            }
                        } else if self.surface == Surface::Settings {
                            egui::ScrollArea::vertical()
                                .id_salt("all-companion-settings")
                                .max_height(self.size[1] - 95.)
                                .show(ui, |ui| {
                                    self.workspace.show(
                                        ui,
                                        &self.client,
                                        self.pane == Pane::Threat,
                                        &mut self.options,
                                    );
                                    ui.separator();
                                    let threat = self.pane == Pane::Threat;
                                    let style = if threat { &mut self.options.threat } else { &mut self.options.ram };
                                    let mut changed = ui.checkbox(&mut style.capture_hidden, "Hide from supported screen capture").changed();
                                    ui.small("Applies to this companion and its speech boxes. Verify with your meeting app; this is not a guarantee for every capture method.");
                                    changed |= ui.checkbox(&mut style.audio_muted, "Mute companion audio").changed();
                                    changed |= ui.checkbox(&mut style.spoken_replies, "Read chat replies aloud").changed();
                                    if changed { self.error = self.options.save().err(); }
                                    ui.horizontal(|ui| {
                                        if ui.add_enabled(self.audio_result.is_none(), egui::Button::new("Test voice")).clicked() {
                                            self.audio_status = Some("Requesting voice...".into());
                                            self.audio_result = Some(super::audio::say(self.client.clone(), threat,
                                                if threat { "ThreatWarden here. Your voice connection is working." } else { "RamWarden here. Your voice connection is working." }.into(), true));
                                        }
                                        if ui.button("Test bleat").clicked() {
                                            self.audio_status = Some(match super::audio::bleat(threat) {
                                                Ok(()) => "Bleat sent to Windows audio.".into(), Err(e) => e,
                                            });
                                        }
                                    });
                                    if let Some(status) = &self.audio_status { ui.label(status); }
                                    ui.separator();
                                    if ui.button("Forget local chat history").clicked() {
                                        self.chat.forget();
                                    }
                                    companion_ui::settings(
                                        ui,
                                        self.pane == Pane::Threat,
                                        &mut self.options,
                                        &mut self.error,
                                    );
                                });
                        } else {
                            self.chat.show(ui, &self.client, self.pane == Pane::Threat);
                        }
                    });
                if self.above {
                    tail(ui, true);
                }
            });
    }
}
fn tail(ui: &mut egui::Ui, down: bool) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 16.), egui::Sense::hover());
    let mid = if down {
        rect.center_top()
    } else {
        rect.center_bottom()
    };
    let sign = if down { 1. } else { -1. };
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            mid + egui::vec2(-10., -2. * sign),
            mid + egui::vec2(0., 14. * sign),
            mid + egui::vec2(10., -2. * sign),
        ],
        egui::Color32::from_rgb(19, 21, 29),
        egui::Stroke::new(2., egui::Color32::from_gray(235)),
    ));
}
fn matching_warden(warden: &str, pane: Pane) -> bool {
    match pane {
        Pane::Ram => warden.is_empty() || warden == "ram",
        Pane::Threat => warden == "threat",
    }
}
pub fn run(client: Client, pane: Pane) -> Result<(), String> {
    let thought = std::env::args().any(|a| a == "--thought");
    let settings = std::env::args().any(|a| a == "--settings");
    let surface = if thought {
        Surface::Thought
    } else if settings {
        Surface::Settings
    } else {
        Surface::Chat
    };
    let size = if thought { [300., 180.] } else { [380., 410.] };
    let (position, above) = pets::attachment(pane, size, thought);
    let title = if pane == Pane::Ram {
        "RamWarden speech"
    } else {
        "ThreatWarden speech"
    };
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_visible(false)
            .with_active(!thought)
            .with_inner_size(size)
            .with_position(position)
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        title,
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            let ctx = cc.egui_ctx.clone();
            let poller = if thought {
                Some(Poller::start(client.clone(), false, move || {
                    ctx.request_repaint()
                }))
            } else {
                None
            };
            Ok(Box::new(CompanionWindow {
                client,
                pane,
                surface,
                reader: Reader::for_companion(pane),
                chat: Chat::for_warden(pane == Pane::Threat),
                options: Options::load(),
                error: None,
                poller,
                started: Instant::now(),
                position,
                size,
                above,
                shown: false,
                workspace: super::workspace_ui::WorkspaceUi::default(),
                audio_result: None,
                audio_status: None,
            }))
        }),
    )
    .map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proactive_speech_is_routed_to_its_own_pet() {
        assert!(matching_warden("", Pane::Ram));
        assert!(matching_warden("ram", Pane::Ram));
        assert!(matching_warden("threat", Pane::Threat));
        assert!(!matching_warden("threat", Pane::Ram));
        assert!(!matching_warden("ram", Pane::Threat));
        assert!(!matching_warden("unknown", Pane::Ram));
    }
}
