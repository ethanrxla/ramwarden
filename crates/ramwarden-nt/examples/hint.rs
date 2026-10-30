//! Set or clear the background hint on processes by PID.
//!
//! The daemon clears every hint it set when it shuts down cleanly. A daemon
//! killed outright cannot, and this is the manual way back:
//!
//! ```text
//! cargo run -p ramwarden-nt --example hint -- off 19956 23216
//! cargo run -p ramwarden-nt --example hint -- on  19956
//! ```

#[cfg(windows)]
fn main() {
    use ramwarden_platform::Background;

    let mut args = std::env::args().skip(1);
    let on = match args.next().as_deref() {
        Some("on") => true,
        Some("off") => false,
        _ => {
            eprintln!("usage: hint on|off <pid>...");
            std::process::exit(2);
        }
    };
    for pid in args.filter_map(|a| a.parse::<i32>().ok()) {
        match ramwarden_nt::platform::NtBackground.set(pid, on) {
            Ok(()) => println!("pid {pid}: background {}", if on { "on" } else { "off" }),
            Err(e) => println!("pid {pid}: {e}"),
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("this example is Windows-only");
}
