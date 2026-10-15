//! Show what the ladder would do on this machine right now. Changes nothing.
//!
//! Run with: cargo run -p ramwarden-core --example ladder

use ramwarden_core::{
    actuator::Actuator,
    config,
    detector::Detector,
    host,
    ladder::{self, Ladder, World, rung_for},
};
use ramwarden_platform::Pressure;

fn mb(b: u64) -> String {
    if b == u64::MAX {
        return "unknown".into();
    }
    format!("{:.0} MB", b as f64 / 1e6)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let platform = host::platform();
    let cfg = config::load().unwrap_or_else(|e| {
        eprintln!("config: {e}");
        config::Config::default()
    });
    println!("platform {}, config from {:?}\n", platform.name, cfg.source);

    let mut det = Detector::new(platform.processes());
    let d = platform.desktop().sample();
    det.sample(&d)?;
    std::thread::sleep(std::time::Duration::from_secs(2));
    det.sample(&d)?;

    let live = platform.pressure.read()?;
    let available = ladder::available_bytes(platform.memory.as_ref());
    let z = platform.memory.compression().unwrap_or_default();

    println!(
        "pressure  some avg10={:.2}%  full avg10={:.2}%{}",
        live.some_avg10,
        live.full_avg10,
        if live.estimated { "  (estimated)" } else { "" }
    );
    println!("available {}", mb(available));
    println!("compressed {} stored in {} ({:.2}x — {} of apparent use is not real)",
        mb(z.original), mb(z.resident), z.ratio(), mb(z.saved()));
    println!("idle      {} across {} processes\n", mb(ladder::idle_bytes(&det)), det.snapshot().len());

    println!("thresholds  reclaim>{} pageout>{} tabs>{} suspend>{} kill(full)>{} or avail<{}MB",
        cfg.ladder.reclaim_at, cfg.ladder.pageout_at, cfg.ladder.tabs_at,
        cfg.ladder.suspend_at, cfg.ladder.kill_at_full, cfg.ladder.kill_below_available_mb);

    let l = Ladder::new(cfg.ladder.clone(), Actuator::for_platform(&platform));

    let w = World {
        det: &det,
        groups: platform.ready_groups(),
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
        let probe = Pressure { some_avg10: p, ..Default::default() };
        println!("    some={p:>5.1}% -> {}", rung_for(&cfg.ladder, &probe, available).as_str());
    }
    let thrash = Pressure { full_avg10: 30.0, ..Default::default() };
    println!("    full= 30.0% -> {}", rung_for(&cfg.ladder, &thrash, available).as_str());
    println!("    avail<500MB -> {}", rung_for(&cfg.ladder, &Default::default(), 400_000_000).as_str());
    Ok(())
}
