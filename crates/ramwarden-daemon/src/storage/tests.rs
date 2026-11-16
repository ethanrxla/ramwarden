use std::fs;
use std::path::Path;

use ramwarden_core::config;

use super::scan::{CACHE_SIGNATURE, CACHE_TAG};
use super::*;

fn write(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![3u8; bytes]).unwrap();
}

const MB: usize = 1_048_576;

/// A Linux home with a big cache, a project with tagged build output, and
/// the user's own files.
fn home() -> (tempfile::TempDir, Storage) {
    let dir = tempfile::tempdir().unwrap();
    let h = dir.path();
    write(&h.join(".cache/browser/blob"), 60 * MB);
    write(&h.join(".npm/_cacache/tiny"), MB);
    write(&h.join("Documents/thesis.pdf"), 5 * MB);
    write(&h.join("Downloads/installer.iso"), 70 * MB);
    write(&h.join("code/app/target/debug/app.rlib"), 210 * MB);
    fs::write(h.join("code/app/target").join(CACHE_TAG), CACHE_SIGNATURE).unwrap();
    write(&h.join("code/app/src/main.rs"), 1_000);
    let storage = Storage::new(Places { os: "linux", home: h.to_path_buf(), ..Default::default() });
    (dir, storage)
}

#[test]
fn a_scan_finds_the_folders_the_known_places_and_tagged_build_output() {
    let (_dir, storage) = home();
    let usage = storage.scan(30, 1_000);
    assert!(!usage.partial);
    assert_eq!(name_of(&usage.folders[0].path), "code");
    let by_id = |id: &str| usage.places.iter().find(|r| r.id == id).unwrap_or_else(|| panic!("{id} missing"));
    assert!(by_id("app-caches").bytes >= 60 * MB as u64);
    assert_eq!(by_id("downloads").kind, Kind::Review);
    let build = by_id("build-1");
    assert_eq!(build.label, "Build output: target in app");
    assert!(build.tagged && build.kind == Kind::Clear);
    // 60 MB of caches and 210 MB of build output; the 1 MB npm cache is too
    // small to offer.
    assert!(usage.clearable() >= 270 * MB as u64);
    assert_eq!(usage.clear_rows().count(), 2);
}

#[test]
fn the_agent_view_has_names_and_sizes_but_no_paths() {
    let (dir, storage) = home();
    let usage = storage.scan(30, 1_000);
    let view = usage.view(false);
    let agent = view.to_string();
    let home = dir.path().to_string_lossy().into_owned();
    assert!(!agent.contains(&home) && !agent.contains(&home.replace('\\', "\\\\")), "a path leaked: {agent}");
    assert!(view["home_folders"][0].get("path").is_none() && view["places"][0].get("paths").is_none());
    assert!(agent.contains("\"Downloads\"") && agent.contains("Build output: target in app"));
    // Compared as values: a Windows path's backslashes are escaped in JSON text.
    let local = usage.view(true);
    assert!(local["home_folders"][0]["path"].as_str().unwrap().starts_with(&home));
    assert!(local["places"][0]["paths"][0].as_str().unwrap().starts_with(&home));
}

#[test]
fn nothing_is_deleted_until_a_proposal_is_applied_and_only_once() {
    let (dir, storage) = home();
    let cfg = config::Storage::default();
    assert_eq!(storage.propose(&[], 1_000).unwrap_err(), Refusal::NotScanned);
    storage.scan(30, 1_000);
    let p = storage.propose(&[], 1_000).unwrap();
    assert_eq!(p.lines.len(), 2);
    assert!(dir.path().join(".cache/browser/blob").exists(), "proposing deletes nothing");

    let (applied, outcome) = storage.apply(&p.id, &cfg, 1_010).unwrap();
    assert!(outcome.bytes_freed >= 270 * MB as u64);
    assert!(matches!(applied.state, State::Applied { freed_bytes, .. } if freed_bytes == outcome.bytes_freed));
    assert!(!dir.path().join(".cache/browser").exists());
    assert!(!dir.path().join("code/app/target/debug").exists());
    // The user's files and the project's source are untouched.
    assert!(dir.path().join("Documents/thesis.pdf").exists());
    assert!(dir.path().join("Downloads/installer.iso").exists());
    assert!(dir.path().join("code/app/src/main.rs").exists());

    assert!(matches!(storage.apply(&p.id, &cfg, 1_020), Err(Refusal::NotPending(State::Applied { .. }))));
    assert_eq!(storage.apply("no-such-id", &cfg, 1_020).unwrap_err(), Refusal::UnknownProposal);
}

#[test]
fn only_clearable_places_can_be_proposed() {
    let (dir, storage) = home();
    storage.scan(30, 1_000);
    assert_eq!(storage.propose(&["downloads".into()], 1_000).unwrap_err(), Refusal::NothingToClear);
    assert_eq!(storage.propose(&["../../etc".into()], 1_000).unwrap_err(), Refusal::NothingToClear);
    let p = storage.propose(&["npm-cache".into(), "downloads".into()], 1_000).unwrap();
    assert_eq!(p.lines.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), vec!["npm-cache"]);
    assert!(dir.path().join("Downloads/installer.iso").exists());
}

#[test]
fn declined_and_expired_proposals_cannot_be_applied() {
    let (dir, storage) = home();
    let cfg = config::Storage::default();
    storage.scan(30, 1_000);
    let no = storage.propose(&[], 1_000).unwrap();
    storage.decline(&no.id, 1_001).unwrap();
    assert_eq!(storage.apply(&no.id, &cfg, 1_002).unwrap_err(), Refusal::NotPending(State::Declined));
    let late = storage.propose(&[], 1_000).unwrap();
    assert_eq!(storage.apply(&late.id, &cfg, 1_000 + 16 * 60).unwrap_err(), Refusal::NotPending(State::Expired));
    assert!(dir.path().join(".cache/browser/blob").exists());
}

#[test]
fn the_alert_and_the_summary_say_the_figures() {
    let (_dir, storage) = home();
    let usage = storage.scan(30, 1_000);
    let reading = Reading { name: "C:".into(), mount: "C:\\".into(), total: 930 << 30, free: 214_748_364, system: true, level: Level::Critical };
    let alert = Alert { volume: reading.clone(), at: 0 };
    let text = alert_text(&alert, usage.clearable());
    assert!(text.starts_with("Baa! C: is nearly full: 0.2 GB left of 930 GB."), "{text}");
    assert!(text.contains("Shall I?"));
    assert!(alert_text(&alert, 0).contains("nothing I can safely clear"));
    let s = summary(&[reading], &usage);
    assert!(s.contains("C: has 0.2 GB free of 930 GB."), "{s}");
    assert!(s.contains("Biggest folders in your home folder: code"), "{s}");
    assert!(s.contains("Yours to decide, I can walk you through each: Downloads"), "{s}");
}
