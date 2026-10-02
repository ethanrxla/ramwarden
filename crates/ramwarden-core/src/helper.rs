//! Client for the privileged helper.
//!
//! The daemon cannot page out another process's memory itself —
//! `process_madvise` needs `CAP_SYS_NICE`, and a systemd *user* service cannot be
//! granted capabilities. `ramwarden-helper` holds that capability and nothing
//! else; this talks to it.
//!
//! Everything here degrades. No socket, no helper running, no capability granted:
//! the call returns an explanatory [`Outcome`] and the ladder falls back to
//! cgroup reclaim, which needs no privileges at all and does most of the work
//! anyway. That fallback is the normal configuration, not a broken one.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use ramwarden_kernel::smaps::Vma;

use crate::actuator::Outcome;

/// How long to wait for the helper. Paging out is quick; a slow answer means
/// something is wrong and the ladder should move on rather than stall its tick.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Where the helper listens.
pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { getuid() })));
    dir.join("ramwarden-helper.sock")
}

/// Whether a helper appears to be listening.
pub fn present() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}

/// Ask the helper to page out these regions of `pid`.
///
/// Returns a refusal rather than an error when the helper is absent: the caller
/// is the ladder, for which "this rung is unavailable" is an ordinary outcome.
pub fn page_out(pid: i32, vmas: &[Vma]) -> Outcome {
    if vmas.is_empty() {
        return Outcome::refused("no cold regions to page out");
    }

    let path = socket_path();
    let stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            return Outcome::refused(format!(
                "helper not running at {} ({e}) — using cgroup reclaim instead",
                path.display()
            ));
        }
    };
    if stream.set_read_timeout(Some(TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(TIMEOUT)).is_err()
    {
        return Outcome::refused("could not set a timeout on the helper socket");
    }

    let request = serde_json::json!({
        "pid": pid,
        "advice": "pageout",
        "ranges": vmas.iter().map(|v| [v.start, v.end]).collect::<Vec<_>>(),
    });

    let mut writer = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => return Outcome::refused(format!("helper socket: {e}")),
    };
    if writeln!(writer, "{request}").is_err() || writer.flush().is_err() {
        return Outcome::refused("helper closed the connection before reading");
    }

    let mut line = String::new();
    if BufReader::new(stream).read_line(&mut line).is_err() || line.trim().is_empty() {
        return Outcome::refused("helper did not answer");
    }

    parse_reply(&line, pid)
}

/// Read the helper's reply.
fn parse_reply(line: &str, pid: i32) -> Outcome {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return Outcome::refused(format!("helper reply was not JSON: {:?}", line.trim()));
    };
    match v.get("status").and_then(|s| s.as_str()) {
        Some("ok") => {
            let bytes = v.get("bytes").and_then(|b| b.as_u64()).unwrap_or(0);
            if bytes == 0 {
                // The syscall succeeded but moved nothing — the pages were
                // already out, or the kernel declined them.
                return Outcome::refused(format!("pid {pid}: nothing was paged out"));
            }
            Outcome {
                affected: vec![pid],
                bytes_freed: bytes,
                notes: Vec::new(),
            }
        }
        Some("error") => Outcome::refused(
            v.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("helper reported an error")
                .to_string(),
        ),
        _ => Outcome::refused(format!("unrecognised helper reply: {:?}", line.trim())),
    }
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn getuid() -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vma(start: u64, end: u64) -> Vma {
        Vma {
            start,
            end,
            rss: end - start,
            referenced: 0,
            anonymous: end - start,
            locked: false,
            private: true,
        }
    }

    #[test]
    fn the_socket_path_is_per_user() {
        let p = socket_path();
        assert!(p.ends_with("ramwarden-helper.sock"), "{p:?}");
    }

    #[test]
    fn no_regions_is_refused_without_connecting() {
        let o = page_out(1234, &[]);
        assert!(o.did_nothing());
        assert!(o.notes[0].contains("no cold regions"));
    }

    /// The expected configuration on a machine where setcap was never run. It
    /// must read as "this rung is unavailable", with the fallback named.
    #[test]
    fn an_absent_helper_is_a_refusal_that_names_the_fallback() {
        if present() {
            return; // a helper really is running; covered by the live path
        }
        let o = page_out(std::process::id() as i32, &[vma(0x1000, 0x2000)]);
        assert!(o.did_nothing());
        assert!(
            o.notes[0].contains("helper not running"),
            "{:?}",
            o.notes
        );
        assert!(
            o.notes[0].contains("cgroup reclaim"),
            "the refusal must say what happens instead: {:?}",
            o.notes
        );
    }

    #[test]
    fn a_successful_reply_reports_the_bytes_moved() {
        let o = parse_reply(r#"{"status":"ok","bytes":524288}"#, 42);
        assert_eq!(o.bytes_freed, 524_288);
        assert_eq!(o.affected, vec![42]);
        assert!(o.notes.is_empty());
    }

    /// A syscall that succeeded but moved nothing is not a success worth
    /// reporting as one — the pages were already out.
    #[test]
    fn a_zero_byte_success_is_reported_as_having_done_nothing() {
        let o = parse_reply(r#"{"status":"ok","bytes":0}"#, 42);
        assert!(o.did_nothing());
        assert!(o.notes[0].contains("nothing was paged out"));
    }

    #[test]
    fn an_error_reply_carries_the_helpers_reason() {
        let o = parse_reply(
            r#"{"status":"error","message":"process_madvise on another process needs CAP_SYS_NICE"}"#,
            42,
        );
        assert!(o.did_nothing());
        assert!(o.notes[0].contains("CAP_SYS_NICE"));
    }

    #[test]
    fn a_malformed_reply_is_refused_rather_than_trusted() {
        for bad in ["not json", "", "{}", r#"{"status":"weird"}"#] {
            let o = parse_reply(bad, 42);
            assert!(o.did_nothing(), "{bad:?} should not look like success");
            assert!(!o.notes.is_empty());
        }
    }
}
