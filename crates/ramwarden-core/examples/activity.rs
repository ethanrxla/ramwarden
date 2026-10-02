//! Sample the live system and print what the detector concludes.
//!
//! Samples twice, because the CPU signal is a delta and the detector refuses to
//! call anything idle until it has two readings.
//!
//! Run with: cargo run -p ramwarden-core --example activity

use ramwarden_core::{desktop::DesktopProbe, detector::Detector, signals::Verdict};
use ramwarden_kernel::Root;

fn mb(b: u64) -> String {
    format!("{:.0}", b as f64 / 1e6)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let watchlist: Vec<String> = ["Discord", "BurpSuiteCommunity", "burpsuite"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let mut det = Detector::new(Root::system());
    let mut probe = DesktopProbe::new();

    let d = probe.sample();
    det.sample(&d)?;
    println!("first sample taken (warm={}) — waiting 2s for a CPU delta", det.is_warm());
    std::thread::sleep(std::time::Duration::from_secs(2));
    det.sample(&d)?;
    println!("second sample taken (warm={})\n", det.is_warm());

    let t = det.totals();
    println!(
        "totals (PSS)   protected {} MB   in use {} MB   idle {} MB   across {} processes\n",
        mb(t["PROTECTED"]), mb(t["IN_USE"]), mb(t["IDLE"]), det.snapshot().len()
    );

    let mut all: Vec<_> = det.snapshot().values().collect();
    all.sort_by_key(|s| std::cmp::Reverse(s.pss));

    println!("{:<22} {:>7} {:>7} {:<10} {:<11} WHY", "NAME", "PSS", "RSS", "VERDICT", "ROLE");
    println!("{}", "-".repeat(118));
    for s in all.iter().filter(|s| s.pss > 150_000_000) {
        println!(
            "{:<22} {:>7} {:>7} {:<10} {:<11} {}",
            s.name,
            mb(s.pss),
            mb(s.rss),
            s.verdict.as_str(),
            s.role.as_str(),
            s.reasons.first().map(String::as_str).unwrap_or("")
        );
    }

    println!("\nRECLAIMABLE (watchlist ∩ idle ∩ worth it):");
    let r = det.reclaimable(&watchlist);
    if r.is_empty() {
        println!("  none this round");
    }
    for s in &r {
        println!("  {} (pid {}) {} MB — {}", s.name, s.pid, mb(s.pss), s.reasons.join("; "));
    }

    println!("\nGATE CHECKS:");
    for name in ["cosmic-comp", "dockerd", "brave", "Discord", "ollama", "not-running"] {
        let d = det.may_suspend(name, &watchlist);
        println!(
            "  {:<14} {:<7} {}",
            name,
            if d.is_allowed() { "ALLOW" } else { "REFUSE" },
            d.reason()
        );
    }

    let protected = all.iter().filter(|s| s.verdict == Verdict::Protected).count();
    println!("\n{protected} processes structurally or soft protected");
    Ok(())
}
