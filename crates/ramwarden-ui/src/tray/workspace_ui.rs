//! Windows folder controls over the same reviewed Rust workspace API as Mac.
use crate::{client::Client, companions::Options};
use eframe::egui;
use serde::Deserialize;
use std::{sync::mpsc, time::Duration};
#[derive(Clone, Deserialize)]
pub struct Workspace {
    pub path: String,
    pub scope_id: String,
}
#[derive(Deserialize)]
struct Envelope {
    workspace: Option<Workspace>,
}
#[derive(Default)]
pub struct WorkspaceUi {
    pub workspace: Option<Workspace>,
    loaded: bool,
    result: Option<mpsc::Receiver<Result<Envelope, String>>>,
    error: Option<String>,
    confirm_auto: bool,
}
impl WorkspaceUi {
    fn request(
        &mut self,
        client: Client,
        threat: bool,
        path: Option<Option<String>>,
        ctx: egui::Context,
    ) {
        let (tx, rx) = mpsc::channel();
        self.result = Some(rx);
        self.error = None;
        std::thread::spawn(move || {
            let warden = if threat { "threat" } else { "ram" };
            let r = crate::runtime::network_runtime()
                .map_err(|e| e.to_string())
                .and_then(|rt| {
                    rt.block_on(async {
                        if let Some(path) = path {
                            client
                                .post::<Envelope>(
                                    "/assistant/workspace",
                                    serde_json::json!({"warden":warden,"path":path}),
                                    Duration::from_secs(10),
                                )
                                .await
                        } else {
                            client
                                .get::<Envelope>(
                                    &format!("/assistant/workspace?warden={warden}"),
                                    Duration::from_secs(10),
                                )
                                .await
                        }
                    })
                    .map_err(|e| e.to_string())
                });
            let _ = tx.send(r);
            ctx.request_repaint();
        });
    }
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        client: &Client,
        threat: bool,
        options: &mut Options,
    ) {
        if !self.loaded {
            self.loaded = true;
            self.request(client.clone(), threat, None, ui.ctx().clone());
        }
        if let Some(rx) = &self.result {
            match rx.try_recv() {
                Ok(r) => {
                    self.result = None;
                    match r {
                        Ok(e) => self.workspace = e.workspace,
                        Err(e) => self.error = Some(e),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.result = None;
                    self.error = Some(
                        "Workspace request ended. Recheck the folder before using tools.".into(),
                    );
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        ui.separator();
        ui.strong("Files and terminal tools");
        ui.label(
            self.workspace
                .as_ref()
                .map(|w| w.path.as_str())
                .unwrap_or("No folder connected"),
        );
        if self.result.is_some() {
            ui.spinner();
        }
        if ui
            .add_enabled(self.result.is_none(), egui::Button::new("Connect folder…"))
            .clicked()
        {
            match choose_folder() {
                Ok(Some(path)) => {
                    let style = if threat {
                        &mut options.threat
                    } else {
                        &mut options.ram
                    };
                    style.auto_scope = None;
                    let _ = options.save();
                    self.request(client.clone(), threat, Some(Some(path)), ui.ctx().clone());
                }
                Ok(None) => {}
                Err(e) => self.error = Some(e),
            }
        }
        if ui
            .add_enabled(
                self.workspace.is_some() && self.result.is_none(),
                egui::Button::new("Disconnect folder"),
            )
            .clicked()
        {
            let style = if threat {
                &mut options.threat
            } else {
                &mut options.ram
            };
            style.auto_scope = None;
            let _ = options.save();
            self.request(client.clone(), threat, Some(None), ui.ctx().clone());
        }
        let style = if threat {
            &mut options.threat
        } else {
            &mut options.ram
        };
        let mut enabled = self
            .workspace
            .as_ref()
            .is_some_and(|w| style.auto_scope.as_deref() == Some(w.scope_id.as_str()));
        if ui
            .add_enabled(
                self.workspace.is_some(),
                egui::Checkbox::new(&mut enabled, "Auto file and terminal tools"),
            )
            .changed()
        {
            if enabled {
                self.confirm_auto = true;
            } else {
                style.auto_scope = None;
                let _ = options.save();
            }
        }
        ui.label(if enabled {
            "Auto · supported tools in this folder"
        } else {
            "Manual · review each operation"
        });
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        if self.confirm_auto {
            let mut approve = false;
            let mut cancel = false;
            egui::Window::new("Enable Auto for this folder?").collapsible(false).show(ui.ctx(),|ui|{ui.label("Allow supported reads, listings, new files/folders and Git diagnostics inside this folder. Existing-file edits and multi-step plans still require review. This does not allow arbitrary commands, deletion or security-setting changes.");if let Some(w)=&self.workspace{ui.label(&w.path);}ui.horizontal(|ui|{approve=ui.button("Enable Auto").clicked();cancel=ui.button("Stay Manual").clicked();});});
            if approve {
                if let Some(w) = &self.workspace {
                    let style = if threat {
                        &mut options.threat
                    } else {
                        &mut options.ram
                    };
                    style.auto_scope = Some(w.scope_id.clone());
                    let _ = options.save();
                }
                self.confirm_auto = false;
            }
            if cancel {
                self.confirm_auto = false;
            }
        }
    }
}
fn choose_folder() -> Result<Option<String>, String> {
    use windows::{
        Win32::{Foundation::ERROR_CANCELLED, System::Com::*, UI::Shell::*},
        core::PCWSTR,
    };
    // SAFETY: COM dialog is created, used and released on this owning UI thread.
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if initialized.is_err() {
            return Err("Could not initialize the Windows folder picker".into());
        }
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        let _com = Com;
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| e.to_string())?;
        dialog
            .SetOptions(
                dialog.GetOptions().map_err(|e| e.to_string())?
                    | FOS_PICKFOLDERS
                    | FOS_FORCEFILESYSTEM
                    | FOS_PATHMUSTEXIST,
            )
            .map_err(|e| e.to_string())?;
        if let Err(e) = dialog.Show(None) {
            if e.code() == windows::core::HRESULT::from_win32(ERROR_CANCELLED.0) {
                return Ok(None);
            }
            return Err(e.to_string());
        }
        let item = dialog.GetResult().map_err(|e| e.to_string())?;
        let path = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .map_err(|e| e.to_string())?;
        let result = PCWSTR(path.0).to_string().map_err(|e| e.to_string());
        CoTaskMemFree(Some(path.0.cast()));
        result.map(Some)
    }
}
