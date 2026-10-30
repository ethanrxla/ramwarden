//! The protocol between unprivileged RamWarden and `ramwarden-service`.
//!
//! One JSON object per line, one request per connection. The vocabulary is
//! closed: the service can do exactly what [`Request`] can express and nothing
//! else, which is what makes running it as SYSTEM defensible — the same design
//! as the Linux setcap helper, which can express only two `madvise` values.

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use ramwarden_platform::{Error, Result};
use serde::{Deserialize, Serialize};

/// Where the service listens.
pub const PIPE_NAME: &str = r"\\.\pipe\ramwarden";

/// Everything the service will do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    /// Purge only the low-priority standby list. See [`crate::standby`].
    PurgeLowPriorityStandby,
    /// Trim a process the user's own token cannot open (an elevated app in
    /// their session). Refused for protected and session-0 processes.
    Trim { pid: u32 },
    /// Apply one machine-wide tweak from a shipped profile, by id.
    TweakApply { id: String },
    /// Revert one machine-wide tweak from its recorded snapshot.
    TweakRevert { id: String },
    /// State of every machine-wide tweak.
    TweakStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok {
        #[serde(default)]
        bytes: u64,
        #[serde(default)]
        detail: serde_json::Value,
    },
    Error {
        message: String,
    },
}

impl Response {
    pub fn ok() -> Self {
        Response::Ok {
            bytes: 0,
            detail: serde_json::Value::Null,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Response::Error {
            message: message.into(),
        }
    }
}

/// Send one request to the service and read its answer.
///
/// An absent service is `Unsupported`, not an error to report loudly: it is
/// the normal state of an install where the user never added the service.
pub fn call(request: &Request) -> Result<Response> {
    let pipe = open_with_retry()?;
    let mut writer = pipe
        .try_clone()
        .map_err(|e| Error::os(format!("pipe: {e}")))?;
    let line = serde_json::to_string(request).map_err(|e| Error::os(e.to_string()))?;
    writeln!(writer, "{line}").map_err(|e| Error::os(format!("pipe write: {e}")))?;
    writer.flush().ok();

    let mut reply = String::new();
    BufReader::new(pipe)
        .read_line(&mut reply)
        .map_err(|e| Error::os(format!("pipe read: {e}")))?;
    serde_json::from_str(reply.trim())
        .map_err(|e| Error::os(format!("unreadable service reply {reply:?}: {e}")))
}

/// Whether the service is installed and answering.
pub fn present() -> bool {
    matches!(call(&Request::Ping), Ok(Response::Ok { .. }))
}

/// All pipe instances can be momentarily busy between connections.
fn open_with_retry() -> Result<std::fs::File> {
    const ERROR_PIPE_BUSY: i32 = 231;
    for _ in 0..20 {
        match std::fs::OpenOptions::new().read(true).write(true).open(PIPE_NAME) {
            Ok(f) => return Ok(f),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::Unsupported("ramwarden-service (not installed or not running)"));
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(Error::denied(PIPE_NAME, "only the interactive user may use the service"));
            }
            Err(e) => return Err(Error::os(format!("{PIPE_NAME}: {e}"))),
        }
    }
    Err(Error::os("ramwarden-service stayed busy"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire format is a contract with an installed service of possibly
    /// another version; pin it.
    #[test]
    fn the_wire_format_is_stable() {
        assert_eq!(serde_json::to_string(&Request::Ping).unwrap(), r#"{"op":"ping"}"#);
        assert_eq!(
            serde_json::to_string(&Request::Trim { pid: 42 }).unwrap(),
            r#"{"op":"trim","pid":42}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::TweakApply { id: "privacy.diagtrack".into() }).unwrap(),
            r#"{"op":"tweak_apply","id":"privacy.diagtrack"}"#
        );
        let r: Response = serde_json::from_str(r#"{"status":"ok","bytes":5}"#).unwrap();
        assert!(matches!(r, Response::Ok { bytes: 5, .. }));
    }

    /// Anything outside the vocabulary is unrepresentable, so it cannot be sent.
    #[test]
    fn unknown_operations_do_not_parse() {
        assert!(serde_json::from_str::<Request>(r#"{"op":"run","cmd":"calc"}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"op":"trim"}"#).is_err());
    }

    #[test]
    fn an_absent_service_is_unsupported_not_an_error() {
        if present() {
            return;
        }
        assert!(matches!(call(&Request::Ping), Err(Error::Unsupported(_))));
    }
}
