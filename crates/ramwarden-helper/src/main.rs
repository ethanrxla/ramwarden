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

mod protocol;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use nix::sys::socket::{GetSockOpt, sockopt};
use ramwarden_kernel::madvise;
use ramwarden_kernel::procfd::PidFd;
use ramwarden_kernel::smaps::Vma;

use crate::protocol::{Request, Response};

/// Where the socket lives. `$XDG_RUNTIME_DIR` is per-user and mode 0700, which
/// is the right place for something only this user may talk to.
fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { getuid() })));
    dir.join("ramwarden-helper.sock")
}

fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RAMWARDEN_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let path = socket_path();
    // A socket left behind by a crashed helper would block the bind.
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;

    let privileged = madvise::available();
    tracing::info!(
        "helper listening on {} (capability {})",
        path.display(),
        if privileged {
            "present"
        } else {
            "ABSENT — every request will be refused; run setcap cap_sys_nice+ep"
        }
    );

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                if let Err(e) = serve(s) {
                    tracing::debug!("connection ended: {e}");
                }
            }
            Err(e) => tracing::warn!("accept failed: {e}"),
        }
    }
    cleanup(&path);
    Ok(())
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Handle one connection: verify the peer, then serve requests line by line.
fn serve(stream: UnixStream) -> std::io::Result<()> {
    if let Err(why) = check_peer(&stream) {
        tracing::warn!("rejecting connection: {why}");
        let mut s = stream;
        let _ = writeln!(s, "{}", json(&Response::error(why)));
        return Ok(());
    }

    let reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle(req),
            Err(e) => Response::error(format!("malformed request: {e}")),
        };
        writeln!(writer, "{}", json(&response))?;
        writer.flush()?;
    }
    Ok(())
}

fn json(r: &Response) -> String {
    serde_json::to_string(r).unwrap_or_else(|_| r#"{"status":"error","message":"unserialisable"}"#.into())
}

/// Only this user may talk to the helper.
///
/// The socket mode already enforces this, but a mode is a single `chmod` away
/// from being wrong and this is a capability-holding process. Checking peer
/// credentials means the guarantee does not rest on filesystem permissions alone.
fn check_peer(stream: &UnixStream) -> Result<(), String> {
    let creds = sockopt::PeerCredentials
        .get(stream)
        .map_err(|e| format!("cannot read peer credentials: {e}"))?;
    let ours = unsafe { getuid() };
    if creds.uid() != ours {
        return Err(format!(
            "peer uid {} is not {ours}",
            creds.uid()
        ));
    }
    Ok(())
}

fn handle(req: Request) -> Response {
    if let Err(why) = req.validate() {
        return Response::error(why.to_string());
    }

    // The caller may only touch its own user's processes. The kernel enforces
    // this too, but refusing here keeps the capability from being the thing that
    // decides.
    match owner_uid(req.pid) {
        Some(uid) if uid == unsafe { getuid() } => {}
        Some(uid) => {
            return Response::error(format!("pid {} belongs to uid {uid}", req.pid));
        }
        None => return Response::error(format!("pid {} does not exist", req.pid)),
    }

    let fd = match PidFd::open(req.pid) {
        Ok(fd) => fd,
        Err(e) => return Response::error(format!("pid {}: {e}", req.pid)),
    };

    // The helper is told which ranges to act on; it does not choose them. The
    // daemon picked them from `smaps`, which is the part that needs the policy.
    let vmas: Vec<Vma> = req
        .ranges
        .iter()
        .map(|&(start, end)| Vma {
            start,
            end,
            rss: end - start,
            referenced: 0,
            anonymous: end - start,
            locked: false,
            private: true,
        })
        .collect();

    let asked = req.span();
    match madvise::advise(&fd, &vmas, req.advice.as_madvise()) {
        Ok(bytes) => {
            tracing::info!(
                "paged out {} KB of {} KB asked from pid {} ({:?})",
                bytes / 1024,
                asked / 1024,
                req.pid,
                req.advice
            );
            Response::Ok { bytes }
        }
        Err(e) => Response::error(format!("pid {}: {e}", req.pid)),
    }
}

/// Owning uid of a process, from the `/proc/<pid>` directory.
fn owner_uid(pid: i32) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(format!("/proc/{pid}")).ok().map(|m| m.uid())
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn getuid() -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Advice;

    #[test]
    fn the_socket_lives_under_the_per_user_runtime_directory() {
        let p = socket_path();
        assert!(p.ends_with("ramwarden-helper.sock"), "{p:?}");
        let parent = p.parent().unwrap().to_string_lossy().into_owned();
        assert!(
            parent.contains("/run/user/") || std::env::var("XDG_RUNTIME_DIR").is_ok(),
            "{parent}"
        );
    }

    #[test]
    fn an_invalid_request_is_refused_without_touching_any_process() {
        let r = handle(Request {
            pid: 1,
            ranges: vec![(0x1000, 0x2000)],
            advice: Advice::PageOut,
        });
        match r {
            Response::Error { message } => assert!(message.contains("pid 1"), "{message}"),
            Response::Ok { .. } => panic!("init must never be targeted"),
        }
    }

    #[test]
    fn a_request_for_a_nonexistent_process_is_refused() {
        let r = handle(Request {
            pid: 0x7FFF_FFFE,
            ranges: vec![(0x1000, 0x2000)],
            advice: Advice::PageOut,
        });
        assert!(matches!(r, Response::Error { .. }));
    }

    /// A process belonging to another user must be refused by the helper itself,
    /// not merely by the kernel — the capability must never be what decides.
    #[test]
    fn a_request_for_another_users_process_is_refused() {
        // pid 2 is kthreadd, owned by root on every Linux system.
        if owner_uid(2) == Some(unsafe { getuid() }) {
            return; // running as root; the check cannot be exercised
        }
        let r = handle(Request {
            pid: 2,
            ranges: vec![(0x1000, 0x2000)],
            advice: Advice::PageOut,
        });
        match r {
            Response::Error { message } => {
                assert!(message.contains("uid") || message.contains("does not exist"), "{message}")
            }
            Response::Ok { .. } => panic!("another user's process must be refused"),
        }
    }

    #[test]
    fn our_own_process_is_accepted_by_the_ownership_check() {
        let me = std::process::id() as i32;
        assert_eq!(owner_uid(me), Some(unsafe { getuid() }));
    }

    #[test]
    fn a_response_always_serialises_even_if_it_has_to_degrade() {
        assert!(json(&Response::Ok { bytes: 1 }).contains("\"ok\""));
        assert!(json(&Response::error("x")).contains("\"error\""));
    }
}
