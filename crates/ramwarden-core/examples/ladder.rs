//! Show what the ladder would do on this machine right now. Changes nothing.
//!
//! Run with: cargo run -p ramwarden-core --example ladder

use ramwarden_core::{
    actuator::Actuator,
    config,
    desktop::DesktopProbe,
    detector::Detector,
    ladder::{self, Ladder, World, rung_for},
};
use ramwarden_kernel::{Root, cgroup, psi, zram};

fn mb(b: u64) -> String {
    if b == u64::MAX {
        return "unknown".into();
    }
    format!("{:.0} MB", b as f64 / 1e6)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Root::system();
    let cfg = config::load().unwrap_or_else(|e| {
        eprintln!("config: {e}");
        config::Config::default()
    });
    println!("config from {:?}\n", cfg.source);

    let mut det = Detector::new(root.clone());
    let mut probe = DesktopProbe::new();
    let d = probe.sample();
    det.sample(&d)?;
    std::thread::sleep(std::time::Duration::from_secs(2));
    det.sample(&d)?;

    let live = psi::system_memory(&root)?;
    let available = ladder::available_bytes(&root);
    let z = zram::total(&root)?;

    println!("PSI       some avg10={:.2}%  full avg10={:.2}%", live.some.avg10, live.full.avg10);
    println!("available {}", mb(available));
    println!("zram      {} stored in {} ({:.2}x — {} of apparent use is not real)",
        mb(z.orig_data_size), mb(z.mem_used_total), z.ratio(), mb(z.saved()));
    println!("idle      {} across {} processes\n", mb(ladder::idle_bytes(&det)), det.snapshot().len());

    println!("thresholds  reclaim>{} pageout>{} tabs>{} suspend>{} kill(full)>{} or avail<{}MB",
        cfg.ladder.reclaim_at, cfg.ladder.pageout_at, cfg.ladder.tabs_at,
        cfg.ladder.suspend_at, cfg.ladder.kill_at_full, cfg.ladder.kill_below_available_mb);

    let uid = unsafe { getuid() };
    let h = cgroup::Hierarchy::user_session(&root, uid).ok();
    let l = Ladder::new(cfg.ladder.clone(), Actuator::new(root.clone()));

    let w = World {
        det: &det,
        hierarchy: h.as_ref(),
        psi: live,
        available_bytes: available,
        watchlist: &cfg.watchlist,
        goal: String::new(),
        tabs: None,
    };

    let plan = l.plan(&w);
    println!("\n==> rung: {}", plan.rung.map(|r| r.as_str()).unwrap_or("none"));
    if let Some(b) = &plan.blocked {
        println!("    blocked: {b}");
    }
    if plan.reclaim.is_empty() {
        println!("    would reclaim: nothing");
    }
    for (name, ask) in &plan.reclaim {
        println!("    would reclaim {:<22} {}", name, mb(*ask));
    }
    if let Some((name, cap)) = &plan.soft_cap {
        println!("    would soft-cap {name} to {}", mb(*cap));
    }
    println!("    would suspend: {:?}", plan.suspend);
    println!("    would kill:    {:?}", plan.kill);

    // What the ladder would decide at each pressure level, on today's machine.
    println!("\nescalation on the current system state:");
    for p in [0.5, 1.5, 3.0, 7.0, 12.0, 20.0] {
        let probe_psi = psi::Psi {
            some: psi::Window { avg10: p, ..Default::default() },
            ..Default::default()
        };
        println!("    some={p:>5.1}% -> {}", rung_for(&cfg.ladder, &probe_psi, available).as_str());
    }
    let thrash = psi::Psi {
        full: psi::Window { avg10: 30.0, ..Default::default() },
        ..Default::default()
    };
    println!("    full= 30.0% -> {}", rung_for(&cfg.ladder, &thrash, available).as_str());
    println!("    avail<500MB -> {}", rung_for(&cfg.ladder, &Default::default(), 400_000_000).as_str());
    Ok(())
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn getuid() -> u32;
}
