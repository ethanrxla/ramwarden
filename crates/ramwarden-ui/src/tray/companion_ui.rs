//! Companion chat uses the same local daemon API and explicit approval gate as Mac.
use crate::{
    client::Client,
    companions::{Options, Pupil},
};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::{sync::mpsc, time::Duration};
#[derive(Clone, Serialize, Deserialize)]
struct Turn {
    role: String,
    content: String,
}
#[derive(Deserialize)]
struct Proposal {
    token: String,
    label: String,
    #[serde(default)]
    preview: String,
}
#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    reply: String,
    proposed_action: Option<Proposal>,
}
#[derive(Default)]
pub struct Chat {
    input: String,
    turns: Vec<Turn>,
    pending: Option<Proposal>,
    approval: bool,
    result: Option<mpsc::Receiver<Result<Reply, String>>>,
    error: Option<String>,
}
impl Chat {
    fn begin(
        &mut self,
        client: Client,
        path: &'static str,
        body: serde_json::Value,
        ctx: egui::Context,
    ) {
        let (tx, rx) = mpsc::channel();
        self.result = Some(rx);
        self.error = None;
        std::thread::spawn(move || {
            let result = crate::runtime::network_runtime()
                .map_err(|e| e.to_string())
                .and_then(|rt| {
                    rt.block_on(client.post::<Reply>(path, body, Duration::from_secs(90)))
                        .map_err(|e| e.to_string())
                });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }
    pub fn show(&mut self, ui: &mut egui::Ui, client: &Client, threat: bool) {
        if let Some(rx) = &self.result {
            match rx.try_recv() {
                Ok(reply) => {
                    self.result = None;
                    match reply {
                        Ok(r) => {
                            if !r.reply.is_empty() {
                                self.turns.push(Turn {
                                    role: "assistant".into(),
                                    content: r.reply,
                                });
                            }
                            self.pending = r.proposed_action;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.result = None;
                    self.error=Some("Connection ended; any action result is unconfirmed. Check before retrying.".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        ui.heading(if threat {
            "ThreatWarden chat"
        } else {
            "RamWarden chat"
        });
        egui::ScrollArea::vertical()
            .id_salt(if threat { "threat-chat" } else { "ram-chat" })
            .stick_to_bottom(true)
            .max_height((ui.available_height() - 210.).max(100.))
            .show(ui, |ui| {
                for t in &self.turns {
                    ui.strong(if t.role == "user" {
                        "You"
                    } else if threat {
                        "ThreatWarden"
                    } else {
                        "RamWarden"
                    });
                    ui.label(&t.content);
                    ui.add_space(8.);
                }
            });
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        let mut dismiss = false;
        if let Some(proposal) = &self.pending {
            ui.separator();
            ui.strong(&proposal.label);
            egui::ScrollArea::vertical()
                .id_salt("proposal")
                .max_height(130.)
                .show(ui, |ui| {
                    ui.label(&proposal.preview);
                });
            ui.horizontal(|ui| {
                if ui.button("Review and approve…").clicked() {
                    self.approval = true;
                }
                if ui.button("Dismiss").clicked() {
                    dismiss = true;
                    self.approval = false;
                }
            });
        }
        if dismiss {
            self.pending = None;
        }
        let busy = self.result.is_some();
        let editor = ui.add_enabled(
            !busy,
            egui::TextEdit::multiline(&mut self.input)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text("Talk to your warden…"),
        );
        let send = ui
            .add_enabled(
                !busy && !self.input.trim().is_empty() && self.input.len() <= 4000,
                egui::Button::new("Send"),
            )
            .clicked()
            || (!busy
                && editor.has_focus()
                && ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Enter)));
        if busy {
            ui.spinner();
        }
        if send && !self.input.trim().is_empty() && self.input.len() <= 4000 {
            let message = self.input.trim().to_owned();
            let history = history(&self.turns);
            self.turns.push(Turn {
                role: "user".into(),
                content: message.clone(),
            });
            self.input.clear();
            self.pending = None;
            self.approval = false;
            self.begin(client.clone(),"/assistant/chat",serde_json::json!({"message":message,"history":history,"warden":if threat{"threat"}else{"ram"}}),ui.ctx().clone());
        }
        if self.approval
            && let Some(p) = &self.pending
        {
            let token = p.token.clone();
            let label = p.label.clone();
            let preview = p.preview.clone();
            let mut confirm = false;
            let mut cancel = false;
            egui::Window::new("Approve this action?")
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.label(label);
                    egui::ScrollArea::vertical()
                        .max_height(280.)
                        .show(ui, |ui| {
                            ui.label(preview);
                        });
                    ui.horizontal(|ui| {
                        confirm = ui.button("Approve once").clicked();
                        cancel = ui.button("Cancel").clicked();
                    });
                });
            if cancel {
                self.approval = false;
            }
            if confirm {
                self.pending = None;
                self.approval = false;
                self.begin(
                    client.clone(),
                    "/assistant/action",
                    serde_json::json!({"token":token}),
                    ui.ctx().clone(),
                );
            }
        }
    }
}
fn history(turns: &[Turn]) -> Vec<Turn> {
    let mut bytes = 0;
    let mut result = vec![];
    for t in turns.iter().rev().take(12) {
        let mut content = t.content.clone();
        while content.len() > 6000 {
            content.pop();
        }
        if bytes + content.len() > 24000 {
            break;
        }
        bytes += content.len();
        result.push(Turn {
            role: t.role.clone(),
            content,
        });
    }
    result.reverse();
    result
}
pub fn settings(
    ui: &mut egui::Ui,
    threat: bool,
    options: &mut Options,
    error: &mut Option<String>,
) {
    ui.heading(if threat {
        "ThreatWarden settings"
    } else {
        "RamWarden settings"
    });
    let style = if threat {
        &mut options.threat
    } else {
        &mut options.ram
    };
    let mut changed = false;
    changed |= ui
        .add(egui::Slider::new(&mut style.scale, 2..=6).text("Size"))
        .changed();
    for (label, color) in [
        ("Wool", &mut style.appearance.wool),
        ("Horns", &mut style.appearance.horns),
        ("Hooves", &mut style.appearance.hooves),
        ("Eyes", &mut style.appearance.eyes),
    ] {
        ui.horizontal(|ui| {
            ui.label(label);
            changed |= ui.color_edit_button_srgb(color).changed();
        });
    }
    if !threat {
        ui.horizontal(|ui| {
            ui.label("Pupils");
            for (label, pupil) in [
                ("Slit", Pupil::Slit),
                ("Round", Pupil::Round),
                ("None", Pupil::None),
            ] {
                changed |= ui
                    .selectable_value(&mut style.appearance.pupil, pupil, label)
                    .changed();
            }
        });
    }
    if ui.button("Restore default appearance").clicked() {
        *style = if threat {
            Options::default().threat
        } else {
            Options::default().ram
        };
        changed = true;
    }
    if changed {
        *error = options.save().err();
    }
    if let Some(e) = error {
        ui.colored_label(egui::Color32::LIGHT_RED, e.as_str());
    }
    ui.label("Changes save on this device and apply to the floating companion within two seconds.");
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_respects_daemon_limits_and_utf8() {
        let t = vec![
            Turn {
                role: "user".into(),
                content: "é".repeat(4000)
            };
            20
        ];
        let h = history(&t);
        assert!(h.len() <= 12);
        assert!(h.iter().all(|t| t.content.len() <= 6000));
        assert!(h.iter().map(|t| t.content.len()).sum::<usize>() <= 24000);
    }
}
