//! `ramwarden-debloat`: see and change the shipped profiles from a terminal.
//!
//! ```text
//! ramwarden-debloat status
//! ramwarden-debloat apply privacy
//! ramwarden-debloat apply background --with-optional
//! ramwarden-debloat apply background.lmstudio-startup
//! ramwarden-debloat revert privacy        # or one id, or `all`
//! ```

#[cfg(windows)]
fn main() {
    use ramwarden_debloat::{Debloat, State};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let debloat = match Debloat::for_user() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot open the debloat journal: {e}");
            std::process::exit(1);
        }
    };
    let print = |lines: Vec<ramwarden_debloat::Line>| {
        let mut failed = false;
        for l in lines {
            failed |= !l.ok;
            println!("  {:<38} {}{}", l.id, if l.ok { "" } else { "FAILED: " }, l.result);
        }
        if failed {
            eprintln!(
                "\nSome machine-wide changes need ramwarden-service (or an elevated terminal)."
            );
            std::process::exit(1);
        }
    };

    match args.first().map(String::as_str) {
        Some("status") | None => {
            let all = debloat.status_all().unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1)
            });
            let mut current = String::new();
            for (profile, s) in all {
                if profile != current {
                    println!("\n[{profile}]");
                    current = profile;
                }
                let mark = match (s.state, s.drifted) {
                    (_, true) => "DRIFTED",
                    (State::Applied, _) if s.recorded => "applied",
                    (State::Applied, _) => "already",
                    (State::Default, _) => "-",
                    (State::Missing, _) => "n/a",
                };
                println!(
                    "  {:<8} {:<38} {:<8}{} {}",
                    mark,
                    s.id,
                    format!("{:?}", s.scope).to_lowercase(),
                    if s.optional { " (optional)" } else { "" },
                    s.description
                );
            }
        }
        Some("apply") if args.len() >= 2 => {
            let optional = args.iter().any(|a| a == "--with-optional");
            match debloat.apply_named(&args[1], optional) {
                Ok(lines) => print(lines),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Some("revert") if args.len() >= 2 => match debloat.revert_named(&args[1]) {
            Ok(lines) => print(lines),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
        _ => {
            eprintln!("usage: ramwarden-debloat status | apply <profile|id> [--with-optional] | revert <profile|id|all>");
            std::process::exit(2);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ramwarden-debloat is Windows-only");
}
