//! Read the live kernel and print what RamWarden v2 now sees.
//!
//! Run with: cargo run -p ramwarden-kernel --example probe

use ramwarden_kernel::{Root, cgroup, psi, smaps};

fn gb(bytes: u64) -> String {
    format!("{:>8.2} GB", bytes as f64 / 1e9)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Root::system();
    let uid = unsafe { libc_getuid() };
    let h = cgroup::Hierarchy::user_session(&root, uid)?;

    let sys = psi::system_memory(&root)?;
    println!("system PSI   some avg10={:.2}%  full avg10={:.2}%", sys.some.avg10, sys.full.avg10);
    println!("session      {}  (user@{}.service)", gb(h.current()?), uid);
    println!();

    let mut scopes: Vec<_> = h
        .scopes()?
        .into_iter()
        .filter_map(|s| s.current().ok().map(|c| (c, s)))
        .collect();
    scopes.sort_by_key(|(c, _)| std::cmp::Reverse(*c));

    println!("{:<26} {:>11} {:>11} {:>11}  SCOPE", "LABEL", "CHARGED", "RECLAIMABLE", "SUM(RSS)");
    println!("{}", "-".repeat(100).as_str());

    let mut tot_charged = 0u64;
    let mut tot_rss = 0u64;

    for (charged, s) in scopes.iter().take(12) {
        let label = s.label(&root).unwrap_or_else(|_| s.name().to_string());
        let reclaimable = s.stat().map(|st| st.reclaimable()).unwrap_or(0);
        // What v1 would have reported for the same processes.
        let rss: u64 = s
            .pids()
            .unwrap_or_default()
            .iter()
            .filter_map(|&p| smaps::rollup(&root, p).ok())
            .map(|r| r.rss)
            .sum();
        tot_charged += charged;
        tot_rss += rss;
        println!(
            "{label:<26} {} {} {}  {}",
            gb(*charged),
            gb(reclaimable),
            gb(rss),
            s.name()
        );
    }

    println!("{}", "-".repeat(100).as_str());
    println!("{:<26} {} {:>11} {}", "TOTAL (top 12)", gb(tot_charged), "", gb(tot_rss));
    println!(
        "\nv1 would have overstated these scopes by {:.2}x",
        tot_rss as f64 / tot_charged.max(1) as f64
    );
    Ok(())
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}
