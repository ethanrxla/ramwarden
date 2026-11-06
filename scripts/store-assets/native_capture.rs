//! Insert as a test-only child of tray in an isolated PR58 worktree.
//! Renders the real WindowApp without opening OS windows or issuing commands.
use super::*;
use egui_kittest::Harness;
use std::{fs,path::PathBuf};

#[test]
#[ignore = "explicit owner-approved recorded-data artifact export only"]
fn recorded_windows_store_assets() {
    let folder=PathBuf::from(std::env::var("RAMWARDEN_STORE_CAPTURE").expect("private capture directory"));
    let read=|name:&str|fs::read_to_string(folder.join(name)).expect("recorded device sample required").trim_start_matches('\u{feff}').to_owned();
    let state=serde_json::from_str(&read("state.json")).expect("valid recorded state");
    let warden:crate::wardens::Warden=serde_json::from_str(&read("threat.json")).expect("valid recorded security report");
    let sprite=crate::wardens::Sprite::from_svg(include_str!("../../../../web/assets/pets/threat.svg"));
    let snapshot=poller::Snapshot {
        state:Some(state),threat:Some(crate::wardens::ThreatPane::from_warden(&warden)),threat_icon:sprite,
        message:Some("Recorded Windows sample • private identifiers redacted • development preview".into()),
        ..Default::default()
    };
    for (name,pane,tab,surface) in [
        ("01-memory",Pane::Ram,Tab::Overview,Surface::Dashboard),
        ("02-processes",Pane::Ram,Tab::Processes,Surface::Dashboard),
        ("03-protection",Pane::Threat,Tab::Overview,Surface::Dashboard),
        ("04-customization",Pane::Ram,Tab::Overview,Surface::Settings),
    ] {
        let mut harness=Harness::builder().with_size(egui::vec2(760.0,620.0)).with_pixels_per_point(2.0).build_eframe(|cc|{
            cc.egui_ctx.set_theme(egui::Theme::Dark);cc.egui_ctx.set_visuals(egui::Visuals::dark());
            WindowApp {
                activation:activation::Reader::default(),surface,
                client:Client::new("127.0.0.1",1).unwrap(),
                chats:std::array::from_fn(|_|companion_ui::Chat::default()),
                options:crate::companions::Options::default(),settings_error:None,
                poller:Poller::recorded_store_snapshot(snapshot.clone()),pane,
                threat_icon:None,tab,view:UiState::default(),
            }
        });
        harness.step();harness.step();
        harness.render().expect("real renderer").save(folder.join(format!("{name}.png"))).expect("save PNG");
    }
}
