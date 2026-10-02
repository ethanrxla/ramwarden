//! Prove the reclaim lever on a live application.
//!
//! Usage: cargo run -p ramwarden-kernel --example reclaim -- <substring> <megabytes>

use ramwarden_kernel::{Root, cgroup, zram};

fn mb(b: u64) -> String {
    format!("{:.0} MB", b as f64 / 1e6)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let want = args.next().unwrap_or_else(|| "brave".into());
    let ask_mb: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(300);

    let root = Root::system();
    let uid = unsafe { getuid() };
    let h = cgroup::Hierarchy::user_session(&root, uid)?;

    // Smallest matching scope, to keep the demonstration gentle.
    let mut cands: Vec<_> = h
        .scopes()?
        .into_iter()
        .filter_map(|s| {
            let label = s.label(&root).unwrap_or_default();
            let hit = label.to_lowercase().contains(&want.to_lowercase())
                || s.name().to_lowercase().contains(&want.to_lowercase());
            hit.then(|| (s.current().unwrap_or(0), label, s))
        })
        .filter(|(c, _, _)| *c > 0)
        .collect();
    cands.sort_by_key(|(c, _, _)| *c);

    let Some((before, label, scope)) = cands.into_iter().next() else {
        println!("no scope matching {want:?}");
        return Ok(());
    };

    let stat = scope.stat()?;
    let z0 = zram::total(&root)?;

    println!("target        {label}  ({})", scope.name());
    println!("charged       {}", mb(before));
    println!("reclaimable   {}  (inactive_anon + inactive_file)", mb(stat.reclaimable()));
    println!("zram stored   {}  in {} of RAM ({:.2}x)", mb(z0.orig_data_size), mb(z0.mem_used_total), z0.ratio());
    println!("\nasking the kernel to reclaim {} ...", mb(ask_mb * 1_000_000));

    let freed = scope.reclaim(ask_mb * 1_000_000)?;
    let after = scope.current()?;
    let z1 = zram::total(&root)?;

    println!("\ncharged now   {}   (dropped {})", mb(after), mb(freed));
    println!("zram stored   {}   (grew {})", mb(z1.orig_data_size), mb(z1.orig_data_size.saturating_sub(z0.orig_data_size)));
    println!("zram RAM      {}   (grew {})", mb(z1.mem_used_total), mb(z1.mem_used_total.saturating_sub(z0.mem_used_total)));

    let net = freed.saturating_sub(z1.mem_used_total.saturating_sub(z0.mem_used_total));
    println!("\nnet RAM returned to the system: {}", mb(net));
    println!("pids still running: {}", scope.pids()?.len());
    Ok(())
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn getuid() -> u32;
}
