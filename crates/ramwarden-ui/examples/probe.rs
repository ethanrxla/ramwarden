//! Drive the daemon client against a live daemon, with no GTK involved.
//! cargo run -p ramwarden-ui --example probe -- [port]

use ramwarden_ui::client::Client;
use ramwarden_ui::model::{self, Direction, SortKey};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(7824);
    let c = Client::new("127.0.0.1", port)?;

    match c.verify().await {
        Ok(v) => println!("daemon at {} is RamWarden {v}", c.base()),
        Err(e) => {
            println!("refused to attach: {e}");
            return Ok(());
        }
    }

    let mut st = c.state().await?;
    println!(
        "\n{:.1}% used — {} of {} (warn at {:.0}%)   PSI some {:.2}%   zram saved {}",
        st.percent,
        model::fmt_mb(st.used_mb),
        model::fmt_mb(st.total_mb),
        st.warn_percent,
        st.psi_some,
        model::fmt_mb(st.zram_saved_mb),
    );
    println!(
        "protected {} · in use {} · idle {} · {} browser(s) · warm {}",
        model::fmt_mb(st.totals_mb.protected),
        model::fmt_mb(st.totals_mb.in_use),
        model::fmt_mb(st.totals_mb.idle),
        st.browsers_connected,
        st.warm,
    );

    // Fold per-process VRAM in, which only /ai knows about.
    if let Ok(ai) = c.ai().await
        && let Some(g) = &ai.gpu
    {
        println!(
            "GPU {} — {} of {} ({:.0}% busy)",
            g.name,
            model::fmt_mb(g.used_mb),
            model::fmt_mb(g.total_mb),
            g.utilisation
        );
    }

    let watchlist = c.watchlist().await.unwrap_or_default();
    println!("watchlist: {watchlist:?}");

    // The table, rendered the way the window will.
    model::sort(&mut st.processes, SortKey::Memory, Direction::Descending);
    let widths: Vec<i32> = model::COLUMNS.iter().map(|c| c.min_width).collect();
    println!("\ncolumn min-widths: {widths:?}  (all resizable: {})",
        model::COLUMNS.iter().filter(|c| !c.title.is_empty()).all(|c| c.resizable));

    println!("\n{:<26} {:>8} {:>10} {:<10} {:<24} WHY", "PROCESS", "PID", "RAM", "STATUS", "SCOPE");
    println!("{}", "-".repeat(120));
    for r in st.processes.iter().take(12) {
        println!(
            "{:<26} {:>8} {:>10} {:<10} {:<24} {}",
            r.name,
            r.pid,
            model::fmt_mb(r.pss_mb),
            r.verdict,
            {
                let s = r.scope_label();
                if s.len() > 23 { s[..23].to_string() } else { s }
            },
            r.reason(),
        );
    }

    // What the context menu would offer for each of the first few rows.
    println!("\ncontext menu by row:");
    for r in st.processes.iter().take(4) {
        let watchlisted = watchlist.iter().any(|w| w.eq_ignore_ascii_case(&r.name));
        println!("  {:<22} protection={:?}", r.name, r.protection);
        for item in model::menu_for(r, watchlisted) {
            match &item.warning {
                Some(w) => println!("       {:<22} ! {w}", item.label()),
                None => println!("       {:<22}", item.label()),
            }
        }
    }

    // Filtering, which is how a user finds anything in 40 rows.
    for f in ["brave", "idle", "4886"] {
        println!("filter {f:?} -> {} row(s)", model::filter(&st.processes, f).len());
    }

    let plan = c.plan().await?;
    println!("\nladder rung: {:?}  blocked: {:?}", plan.rung, plan.blocked);
    if let Some(p) = plan.kill_pending {
        println!("  KILL ARMED on {:?} in {}s", p.targets, p.seconds_remaining);
    }

    let log = c.actions().await?;
    println!("action log: {} row(s), {} reclaimed in total",
        log.actions.len(), model::fmt_mb(log.total_reclaimed_mb));
    Ok(())
}
