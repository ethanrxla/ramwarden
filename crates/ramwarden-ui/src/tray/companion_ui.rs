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
    #[serde(default)]
    scope_id: Option<String>,
    #[serde(default)]
    auto_eligible: bool,
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
    path: Option<std::path::PathBuf>,
    audio_result: Option<mpsc::Receiver<Result<(), String>>>,
}
impl Chat {
    pub fn for_warden(threat: bool) -> Self {
        let path = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(if threat {
                "ramwarden/companion-chat-threat.json"
            } else {
                "ramwarden/companion-chat-ram.json"
            });
        Self::load(path)
    }
    fn load(path: std::path::PathBuf) -> Self {
        let turns: Vec<Turn> = std::fs::read(&path)
            .ok()
            .filter(|b| b.len() <= 1300000)
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            turns: bounded_turns(turns),
            path: Some(path),
            ..Default::default()
        }
    }
    pub fn forget(&mut self) {
        self.result = None;
        self.input.clear();
        self.error = None;
        self.turns.clear();
        self.pending = None;
        self.approval = false;
        self.save();
    }
    fn save(&mut self) {
        self.turns = bounded_turns(std::mem::take(&mut self.turns));
        let Some(path) = &self.path else {
            return;
        };
        let result = (|| -> Result<(), String> {
            std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            let pending = path.with_extension("tmp");
            let mut bytes = serde_json::to_vec(&self.turns).map_err(|e| e.to_string())?;
            while bytes.len() > 1300000 && self.turns.len() > 1 {
                self.turns.remove(0);
                bytes = serde_json::to_vec(&self.turns).map_err(|e| e.to_string())?;
            }
            std::fs::write(&pending, bytes).map_err(|e| e.to_string())?;
            std::fs::rename(pending, path).map_err(|e| e.to_string())
        })();
        if let Err(e) = result {
            self.error = Some(format!("Could not save local conversation: {e}"));
        }
    }

    fn begin(
        &mut self,
        client: Client,
        path: &'static str,
        body: serde_json::Value,
        ctx: egui::Context,
    ) {
        self.error = None;
        if path == "/assistant/action" {
            self.turns.push(Turn{role:"assistant".into(),content:"Approved action submitted. Result is unconfirmed until the warden replies; closing chat does not undo completed changes.".into()});
        }
        self.save();
        let (tx, rx) = mpsc::channel();
        self.result = Some(rx);
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
        if let Some(rx) = &self.audio_result
            && let Ok(result) = rx.try_recv()
        {
            self.audio_result = None;
            if let Err(e) = result {
                self.error = Some(e);
            }
        }

        if let Some(rx) = &self.result {
            match rx.try_recv() {
                Ok(reply) => {
                    self.result = None;
                    match reply {
                        Ok(r) => {
                            if !r.reply.is_empty() {
                                let options = Options::load();
                                let style = if threat { options.threat } else { options.ram };
                                if style.spoken_replies && !style.audio_muted {
                                    self.audio_result = Some(super::audio::say(
                                        client.clone(),
                                        threat,
                                        r.reply.clone(),
                                        false,
                                    ));
                                }

                                self.turns.push(Turn {
                                    role: "assistant".into(),
                                    content: r.reply,
                                });
                            }
                            self.pending = r.proposed_action;
                            self.save();
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
        let options = Options::load();
        let auto_scope = if threat {
            options.threat.auto_scope
        } else {
            options.ram.auto_scope
        };
        if self.result.is_none()
            && let Some(proposal) = &self.pending
            && auto_allowed(proposal, auto_scope.as_deref())
        {
            let token = proposal.token.clone();
            self.pending = None;
            self.approval = false;
            self.begin(
                client.clone(),
                "/assistant/action",
                serde_json::json!({"token":token}),
                ui.ctx().clone(),
            );
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
fn auto_allowed(proposal: &Proposal, scope: Option<&str>) -> bool {
    proposal.auto_eligible
        && proposal
            .scope_id
            .as_deref()
            .is_some_and(|id| !id.is_empty() && Some(id) == scope)
}
fn bounded_turns(turns: Vec<Turn>) -> Vec<Turn> {
    turns
        .into_iter()
        .rev()
        .filter(|t| matches!(t.role.as_str(), "user" | "assistant"))
        .take(200)
        .map(|mut t| {
            while t.content.len() > 6000 {
                t.content.pop();
            }
            t
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
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
        let defaults = if threat {
            Options::default().threat
        } else {
            Options::default().ram
        };
        style.appearance = defaults.appearance;
        style.scale = defaults.scale;
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
    fn auto_requires_backend_eligibility_and_exact_folder_scope() {
        let mut p = Proposal {
            token: "t".into(),
            label: "x".into(),
            preview: String::new(),
            scope_id: Some("folder-a".into()),
            auto_eligible: true,
        };
        assert!(auto_allowed(&p, Some("folder-a")));
        assert!(!auto_allowed(&p, Some("folder-b")));
        assert!(!auto_allowed(&p, None));
        p.auto_eligible = false;
        assert!(!auto_allowed(&p, Some("folder-a")));
    }
    #[test]
    fn escaped_history_stays_within_reload_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ram.json");
        let mut chat = Chat::load(path.clone());
        chat.turns = vec![
            Turn {
                role: "user".into(),
                content: "\tX".repeat(3000)
            };
            200
        ];
        chat.save();
        assert!(std::fs::metadata(&path).unwrap().len() <= 1300000);
        let count = chat.turns.len();
        assert_eq!(Chat::load(path).turns.len(), count);
    }
    #[test]
    fn conversation_survives_close_and_reopen_without_restoring_approval() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ram.json");
        let mut chat = Chat::load(path.clone());
        chat.turns.push(Turn {
            role: "user".into(),
            content: "Keep my conversation".into(),
        });
        chat.pending = Some(Proposal {
            token: "old-approval".into(),
            label: "Never replay".into(),
            preview: "old".into(),
            scope_id: None,
            auto_eligible: false,
        });
        chat.save();
        let restored = Chat::load(path);
        assert_eq!(restored.turns.len(), 1);
        assert_eq!(restored.turns[0].content, "Keep my conversation");
        assert!(restored.pending.is_none());
        assert!(restored.result.is_none());
    }
    #[test]
    fn stored_history_is_bounded_and_rejects_untrusted_roles() {
        let mut turns = vec![
            Turn {
                role: "assistant".into(),
                content: "é".repeat(4000)
            };
            250
        ];
        turns.push(Turn {
            role: "system".into(),
            content: "Not a saved conversation turn".into(),
        });
        let turns = bounded_turns(turns);
        assert_eq!(turns.len(), 200);
        assert!(
            turns
                .iter()
                .all(|t| t.content.len() <= 6000 && t.role == "assistant")
        );
    }
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
