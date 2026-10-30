//! A minimal privileged helper, so the daemon and the UI stay unprivileged.
//!
//! # Why this exists as a separate process
//!
//! `process_madvise(MADV_PAGEOUT)` on another process needs `CAP_SYS_NICE`.
//! Measured on the target machine: it works on the caller's own memory and is
//! refused with `EPERM` on anything else.
//!
//! A systemd *user* service cannot be granted capabilities — the user manager is
//! itself unprivileged — so `AmbientCapabilities=` in `ramwarden.service` does
//! nothing. The remaining options are `setcap` on a binary, or nothing.
//!
//! `setcap cap_sys_nice+ep` on the *daemon* would mean a privileged HTTP server
//! with a WebSocket, a SQLite database, a model client, and a GTK process beside
//! it. This instead is a few hundred lines that accept one message shape and make
//! one syscall. `CAP_SYS_NICE` also permits scheduler and NUMA-migration
//! operations the daemon has no use for; keeping it here means the daemon never
//! holds it.
//!
//! # Threat model
//!
//! The socket is mode 0600 in `$XDG_RUNTIME_DIR` and every connection's peer
//! credentials are checked against the helper's own uid. The user can already do
//! whatever they like to their own processes, so the capability grants no new
//! authority *over the user's own session*; what it must not become is a way for
//! anything else to reach it. Paging out is non-destructive by construction —
//! pages fault back in — and the only two advice values the protocol can express
//! are the non-destructive ones.
//!
//! # Installing
//!
//! ```text
//! sudo setcap cap_sys_nice+ep /usr/bin/ramwarden-helper
//! ```
//!
//! Without that it still runs and still answers, reporting `EPERM` for every
//! request — which is exactly what the daemon's fallback expects.
//!
//! Linux only. On Windows the same job — privileged operations on behalf of an
//! unprivileged daemon — is done by `ramwarden-service`.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::io::Result<()> {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("ramwarden-helper is Linux-only; on Windows, install ramwarden-service");
}
