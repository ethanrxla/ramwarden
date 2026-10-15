//! The detector and the ladder driven end to end through the platform traits.
//!
//! The Linux tests in `ladder.rs` drive a synthetic cgroup tree; these drive
//! `ramwarden_platform::fake`, so the same safety properties are checked on
//! every OS — including Windows, where there is no `/proc` to synthesise.

use std::sync::{Arc, Mutex};

use ramwarden_core::actuator::Actuator;
use ramwarden_core::config;
use ramwarden_core::detector::Detector;
use ramwarden_core::ladder::{Ladder, Rung, World};
use ramwarden_core::signals::Verdict;
use ramwarden_platform::fake::{self, FakeGroup, Shared};
use ramwarden_platform::{MemoryStatus, Owner, Platform, Pressure};

const MB: u64 = 1024 * 1024;

/// An idle Brave (two processes, one group), an idle Discord, a game, and
/// the desktop shell — the target machine's shape.
fn world() -> Shared {
    let mut w = fake::World::default();
    let mut add = |pid, name: &str, mb: u64, group: Option<&str>| {
        let mut p = fake::process(pid, name, mb * MB);
        p.group = group.map(String::from);
        w.processes.push(p);
    };
    add(10, "brave", 900, Some("brave#10"));
    add(11, "brave", 600, Some("brave#10"));
    add(20, "Discord", 500, Some("Discord#20"));
    add(30, "FortniteClient-Win64-Shipping", 3000, Some("FortniteClient-Win64-Shipping#30"));
    add(40, "explorer", 300, Some("explorer#40"));

    for (name, pids, cur) in [
        ("brave#10", vec![10, 11], 1500),
        ("Discord#20", vec![20], 500),
        ("FortniteClient-Win64-Shipping#30", vec![30], 3000),
        ("explorer#40", vec![40], 300),
    ] {
        w.groups.push(FakeGroup {
            name: name.into(),
            pids,
            current: cur * MB,
            reclaimable: cur * MB,
            cap: None,
        });
    }
    w.groups_ready = true;
    w.memory = MemoryStatus {
        total: 32_000 * MB,
        available: 8_000 * MB,
        ..Default::default()
    };
    Arc::new(Mutex::new(w))
}

fn warm_detector(p: &Platform, shared: &Shared) -> Detector {
    let mut det = Detector::new(p.processes());
    let desktop = shared.lock().unwrap().desktop.clone();
    det.sample(&desktop).unwrap();
    det.sample(&desktop).unwrap();
    det
}

fn pressure(some: f64, full: f64) -> Pressure {
    Pressure {
        some_avg10: some,
        full_avg10: full,
        estimated: true,
        ..Default::default()
    }
}

fn step(l: &mut Ladder, p: &Platform, det: &Detector, some: f64, full: f64, wl: &[String]) -> ramwarden_core::ladder::Report {
    let mut w = World {
        det,
        groups: p.ready_groups(),
        psi: pressure(some, full),
        available_bytes: 8_000 * MB,
        watchlist: wl,
        goal: String::new(),
        tabs: None,
    };
    l.step(&mut w)
}

#[test]
fn the_detector_differences_cpu_and_calls_quiet_apps_idle() {
    let shared = world();
    let p = fake::platform(shared.clone());
    let det = warm_detector(&p, &shared);
    assert!(det.is_warm());
    assert_eq!(det.get(10).unwrap().verdict, Verdict::Idle);
    assert_eq!(det.get(30).unwrap().verdict, Verdict::Protected, "a game is structural");
    assert_eq!(det.get(40).unwrap().verdict, Verdict::Protected, "the shell is structural");

    // Busy between samples: 2 CPU-seconds in one tick.
    let mut det = Detector::new(p.processes());
    det.sample(&Default::default()).unwrap();
    shared.lock().unwrap().processes.iter_mut().find(|x| x.pid == 20).unwrap().cpu_seconds += 2.0;
    det.sample(&Default::default()).unwrap();
    assert_eq!(det.get(20).unwrap().verdict, Verdict::InUse);
}

#[test]
fn system_owned_processes_are_protected_whatever_their_name() {
    let shared = world();
    {
        let mut w = shared.lock().unwrap();
        let mut svc = fake::process(50, "some-vendor-updater", 400 * MB);
        svc.owner = Owner::System;
        w.processes.push(svc);
    }
    let p = fake::platform(shared.clone());
    let det = warm_detector(&p, &shared);
    assert_eq!(det.get(50).unwrap().verdict, Verdict::Protected);
}

#[test]
fn reclaim_targets_idle_groups_and_never_a_protected_one() {
    let shared = world();
    let p = fake::platform(shared.clone());
    let det = warm_detector(&p, &shared);
    let mut l = Ladder::new(config::Ladder::default(), Actuator::for_platform(&p));

    let report = step(&mut l, &p, &det, 3.0, 0.0, &[]);
    assert_eq!(report.rung, Some(Rung::Reclaim));

    let w = shared.lock().unwrap();
    assert_eq!(fake::count(&w, "reclaim brave#10"), 1, "{:?}", w.log);
    assert_eq!(fake::count(&w, "reclaim Discord#20"), 1, "{:?}", w.log);
    assert_eq!(fake::count(&w, "reclaim Fortnite"), 0, "a game must never be reclaimed from");
    assert_eq!(fake::count(&w, "reclaim explorer"), 0, "the shell must never be reclaimed from");
    assert!(report.total_freed > 0);
}

#[test]
fn the_game_is_never_a_target_at_any_rung_even_when_watchlisted() {
    let shared = world();
    let p = fake::platform(shared.clone());
    let det = warm_detector(&p, &shared);
    let mut cfg = config::Ladder::default();
    cfg.kill_grace_seconds = 0;
    let mut l = Ladder::new(cfg, Actuator::for_platform(&p));
    let wl = vec!["FortniteClient-Win64-Shipping".to_string(), "Discord".to_string()];

    for (some, full) in [(2.0, 0.0), (6.0, 0.0), (11.0, 0.0), (16.0, 0.0), (1.0, 30.0)] {
        step(&mut l, &p, &det, some, full, &wl);
    }
    let w = shared.lock().unwrap();
    for entry in &w.log {
        assert!(!entry.contains(" 30") && !entry.contains("Fortnite"), "the game was touched: {entry}");
    }
    assert!(!w.exited.contains(&30) && !w.suspended.contains(&30));
}

#[test]
fn suspend_then_release_wakes_exactly_what_was_frozen() {
    let shared = world();
    let p = fake::platform(shared.clone());
    let det = warm_detector(&p, &shared);
    let mut l = Ladder::new(config::Ladder::default(), Actuator::for_platform(&p));
    let wl = vec!["Discord".to_string()];

    let report = step(&mut l, &p, &det, 16.0, 0.0, &wl);
    assert_eq!(report.rung, Some(Rung::Suspend));
    assert_eq!(shared.lock().unwrap().suspended, vec![20]);

    let report = step(&mut l, &p, &det, 0.0, 0.0, &wl);
    assert_eq!(report.rung, Some(Rung::Release));
    let w = shared.lock().unwrap();
    assert!(w.suspended.is_empty(), "release must resume what the ladder froze");
    assert!(w.groups.iter().all(|g| g.cap.is_none()), "and lift every cap");
}

#[test]
fn a_soft_cap_is_set_at_pageout_and_lifted_on_release() {
    let shared = world();
    let p = fake::platform(shared.clone());
    let det = warm_detector(&p, &shared);
    let mut l = Ladder::new(config::Ladder::default(), Actuator::for_platform(&p));

    step(&mut l, &p, &det, 6.0, 0.0, &[]);
    assert!(l.actuator_has_caps());
    assert!(shared.lock().unwrap().groups.iter().any(|g| g.cap.is_some()));

    step(&mut l, &p, &det, 0.0, 0.0, &[]);
    assert!(!l.actuator_has_caps());
    assert!(shared.lock().unwrap().groups.iter().all(|g| g.cap.is_none()));
}

#[test]
fn without_group_reclaim_the_rung_refuses_and_says_why() {
    let shared = world();
    shared.lock().unwrap().groups_ready = false;
    let p = fake::platform(shared.clone());
    let det = warm_detector(&p, &shared);
    let mut l = Ladder::new(config::Ladder::default(), Actuator::for_platform(&p));
    let report = step(&mut l, &p, &det, 3.0, 0.0, &[]);
    assert!(report.steps[0].2.notes[0].contains("group reclaim unavailable"));
}

#[test]
fn estimated_pressure_is_logged_as_an_estimate() {
    let shared = world();
    let p = fake::platform(shared.clone());
    let det = warm_detector(&p, &shared);
    let mut l = Ladder::new(config::Ladder::default(), Actuator::for_platform(&p))
        .with_history(ramwarden_core::history::History::in_memory().unwrap());
    step(&mut l, &p, &det, 3.0, 0.0, &[]);
    let rows = l.history().unwrap().recent_actions(10).unwrap();
    assert!(rows.iter().all(|r| r.trigger == "pressure-estimate"), "{rows:?}");
}
