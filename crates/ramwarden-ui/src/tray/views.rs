//! The four panels. Each draws from a [`Snapshot`] and returns at most one
//! [`Command`]; none of them touches the network.

use std::collections::HashSet;

use eframe::egui::{self, Color32, RichText};

use super::poller::{Command, Snapshot};
use crate::model::{self, Action, Direction, SortKey};

/// View state that survives between frames.
#[derive(Default)]
pub struct UiState {
    pub filter: String,
    /// A command waiting for the user to confirm it, with the question.
    pub confirm: Option<(Command, String)>,
    /// Profiles whose optional tweaks are included in "Apply".
    pub with_optional: HashSet<String>,
}

fn colour(hex: &str) -> Color32 {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x9ca3af);
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn mb(v: f64) -> String {
    model::fmt_mb(v)
}

/// No `/state` yet: say why, and what is being done about it, rather than
/// an open-ended "waiting" that looks the same whether the daemon is starting,
/// crashed, or was never installed. The raw error is in the status bar.
fn waiting(ui: &mut egui::Ui, snap: &Snapshot) {
    ui.label(snap.daemon.as_deref().unwrap_or("Waiting for the daemon…"));
}

// ── Overview ────────────────────────────────────────────────────────────────

pub fn overview(ui: &mut egui::Ui, snap: &Snapshot) {
    let Some(st) = &snap.state else {
        waiting(ui, snap);
        return;
    };
    let fraction = (st.percent / 100.0) as f32;
    ui.add(
        egui::ProgressBar::new(fraction)
            .text(format!("{} of {} in use ({:.0}%)", mb(st.used_mb), mb(st.total_mb), st.percent)),
    );
    ui.add_space(6.0);
    egui::Grid::new("overview").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
        let pressure = if st.pressure_estimated { " (estimated)" } else { "" };
        ui.label("Memory pressure");
        ui.label(format!("{:.1}% some · {:.1}% full{pressure}", st.psi_some, st.psi_full));
        ui.end_row();
        ui.label("Protected / in use / idle");
        ui.label(format!(
            "{} / {} / {}",
            mb(st.totals_mb.protected),
            mb(st.totals_mb.in_use),
            mb(st.totals_mb.idle)
        ));
        ui.end_row();
        ui.label("Saved by compression");
        ui.label(mb(st.zram_saved_mb));
        ui.end_row();
        ui.label("Browsers connected");
        ui.label(st.browsers_connected.to_string());
        ui.end_row();
        if !st.warm {
            ui.label("");
            ui.colored_label(colour("#facc15"), "Still sampling — nothing is called idle yet.");
            ui.end_row();
        }
    });

    ui.separator();
    match &snap.tidy {
        Some(t) if t.enabled => {
            ui.heading("Tidy");
            ui.label(format!(
                "Apps quiet for {:.0}+ min are deprioritised and trimmed once; any use undoes it.",
                t.after_minutes
            ));
            if t.tracking.is_empty() {
                ui.label("Nothing quiet enough yet.");
            }
            egui::Grid::new("tidy").striped(true).num_columns(4).show(ui, |ui| {
                for e in t.tracking.iter().take(12) {
                    ui.label(&e.name);
                    ui.label(mb(e.pss_mb));
                    ui.label(format!("quiet {:.0} min", e.quiet_minutes));
                    ui.label(match (e.hinted, e.trimmed) {
                        (true, true) => "background · trimmed",
                        (true, false) => "background",
                        (false, true) => "trimmed",
                        _ => "waiting",
                    });
                    ui.end_row();
                }
            });
        }
        Some(_) => {
            ui.label("Tidy is disabled in the config.");
        }
        None => {}
    }

    if !st.suspended.is_empty() {
        ui.separator();
        ui.heading("Suspended by RamWarden");
        for s in &st.suspended {
            ui.label(format!("{} — {} for {:.0} min", s.name, mb(s.rss_mb), s.minutes));
        }
    }
}

// ── Processes ───────────────────────────────────────────────────────────────

pub fn processes(ui: &mut egui::Ui, snap: &Snapshot, view: &mut UiState) -> Option<Command> {
    let Some(st) = &snap.state else {
        waiting(ui, snap);
        return None;
    };
    ui.horizontal(|ui| {
        ui.label("Filter");
        ui.text_edit_singleline(&mut view.filter);
    });
    let mut rows = model::filter(&st.processes, &view.filter);
    model::sort(&mut rows, SortKey::Memory, Direction::Descending);

    let mut out = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("procs").striped(true).num_columns(5).show(ui, |ui| {
            for h in ["Process", "RAM", "Status", "Role", "Why"] {
                ui.strong(h);
            }
            ui.end_row();
            for row in &rows {
                let name = ui.add(egui::Label::new(&row.name).sense(egui::Sense::click()));
                ui.label(mb(row.pss_mb));
                ui.colored_label(colour(model::verdict_colour(&row.verdict)), &row.verdict);
                ui.label(&row.role);
                ui.label(row.reason());
                ui.end_row();

                name.context_menu(|ui| {
                    for item in model::menu_for(row, false) {
                        let cmd = match item.action {
                            Action::Reclaim => row.scope.clone().map(Command::Reclaim),
                            Action::Suspend => Some(Command::Suspend { name: row.name.clone(), force: true }),
                            Action::Resume => Some(Command::Resume(row.name.clone())),
                            Action::Kill => Some(Command::Kill { name: row.name.clone(), force: true }),
                            Action::CopyPid => {
                                if ui.button(item.label()).clicked() {
                                    ui.ctx().copy_text(row.pid.to_string());
                                    ui.close();
                                }
                                None
                            }
                            // Watchlist edits belong in the config file.
                            Action::Watch | Action::Spare => None,
                        };
                        if let Some(cmd) = cmd
                            && ui.button(item.label()).clicked()
                        {
                            if item.needs_confirmation() {
                                let why = item.warning.clone().unwrap_or_else(|| "Unsaved work is lost.".into());
                                view.confirm = Some((cmd, format!("{} {}? {why}", item.label(), row.name)));
                            } else {
                                out = Some(cmd);
                            }
                            ui.close();
                        }
                    }
                });
            }
        });
    });
    out
}

// ── Debloat ─────────────────────────────────────────────────────────────────

pub fn debloat(ui: &mut egui::Ui, snap: &Snapshot, view: &mut UiState) -> Option<Command> {
    let Some(d) = &snap.debloat else {
        ui.label("Loading profiles…");
        return None;
    };
    if !d.available {
        ui.label(d.error.clone().unwrap_or_else(|| "Profiles are unavailable on this platform.".into()));
        return None;
    }
    if !d.service_present && !d.elevated {
        ui.colored_label(
            colour("#facc15"),
            "ramwarden-service is not running: per-user tweaks work, machine-wide ones will fail until it is installed.",
        );
    }
    ui.label("Every change is snapshotted first and can be reverted. Defender, Windows Update, SysMain, Search and anti-cheat are never touched.");

    let mut out = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        for p in &d.profiles {
            ui.separator();
            ui.horizontal(|ui| {
                ui.heading(&p.name);
                let mut with = view.with_optional.contains(&p.name);
                if ui.checkbox(&mut with, "include optional").changed() {
                    if with {
                        view.with_optional.insert(p.name.clone());
                    } else {
                        view.with_optional.remove(&p.name);
                    }
                }
                if ui.button("Apply").clicked() {
                    out = Some(Command::DebloatApply { name: p.name.clone(), include_optional: with });
                }
                if ui.button("Revert").clicked() {
                    out = Some(Command::DebloatRevert(p.name.clone()));
                }
            });
            ui.label(&p.description);
            egui::Grid::new(&p.name).striped(true).num_columns(3).show(ui, |ui| {
                for t in &p.tweaks {
                    let (text, c) = match (t.state.as_str(), t.recorded, t.drifted) {
                        (_, _, true) => ("drifted", "#f87171"),
                        ("applied", true, _) => ("applied", "#4ade80"),
                        ("applied", false, _) => ("already set", "#60a5fa"),
                        ("missing", _, _) => ("not present", "#9ca3af"),
                        _ => ("default", "#9ca3af"),
                    };
                    ui.colored_label(colour(c), text);
                    let label = if t.optional { format!("{} (optional)", t.description) } else { t.description.clone() };
                    ui.label(label).on_hover_text(&t.target);
                    ui.horizontal(|ui| {
                        if t.state != "applied" && t.state != "missing" && ui.small_button("apply").clicked() {
                            out = Some(Command::DebloatApply { name: t.id.clone(), include_optional: true });
                        }
                        if t.recorded && ui.small_button("revert").clicked() {
                            out = Some(Command::DebloatRevert(t.id.clone()));
                        }
                    });
                    ui.end_row();
                }
            });
        }
    });
    out
}

// ── GPU ─────────────────────────────────────────────────────────────────────

pub fn gpu(ui: &mut egui::Ui, snap: &Snapshot) -> Option<Command> {
    let Some(g) = &snap.gpu else {
        ui.label("Loading…");
        return None;
    };
    let mut out = None;
    match &g.gpu {
        Some(card) => {
            ui.heading(&card.name);
            ui.add(egui::ProgressBar::new((card.percent_used / 100.0) as f32).text(format!(
                "{} / {} MB VRAM ({:.0}%)",
                card.used_mb, card.total_mb, card.percent_used
            )));
        }
        None => {
            ui.label("No NVIDIA GPU detected.");
        }
    }
    if let Some(game) = &g.game {
        ui.label(RichText::new(format!("Game running: {game} — idle local models are unloaded if VRAM runs short.")).italics());
    }

    ui.separator();
    ui.horizontal(|ui| {
        ui.heading("Loaded local models");
        if !g.models.is_empty() && ui.button("Unload all").clicked() {
            out = Some(Command::GpuUnload(None));
        }
    });
    if g.models.is_empty() {
        ui.label("None (LM Studio and Ollama have nothing loaded).");
    }
    for m in &g.models {
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} · {}{}",
                m.host,
                m.name,
                m.bytes.map(|b| format!(" · {} MB", b / 1_000_000)).unwrap_or_default()
            ));
            if ui.small_button("unload").clicked() {
                out = Some(Command::GpuUnload(Some(m.name.clone())));
            }
        });
    }

    ui.separator();
    ui.heading("VRAM by process");
    egui::Grid::new("vram").striped(true).num_columns(2).show(ui, |ui| {
        for p in &g.processes {
            ui.label(p.name.clone().unwrap_or_else(|| format!("pid {}", p.pid)));
            ui.label(format!("{} MB", p.vram_mb));
            ui.end_row();
        }
    });
    out
}
