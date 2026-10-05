use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

/// Why a platform operation did not happen.
///
/// Four cases, because callers treat them differently: a vanished process is
/// routine, a permission boundary is a configuration to report, an absent
/// interface means degrade, and anything else is a real failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The process, group or file is gone — usually it exited mid-operation.
    #[error("{0}: not found")]
    Missing(String),

    /// The interface exists but this process may not use it.
    #[error("{what}: permission denied ({hint})")]
    Denied { what: String, hint: String },

    /// This platform does not offer the interface at all. Callers degrade.
    #[error("{0} is not available on this platform")]
    Unsupported(&'static str),

    /// Anything else, with the OS's own description.
    #[error("{0}")]
    Os(String),
}

impl Error {
    pub fn missing(what: impl fmt::Display) -> Self {
        Error::Missing(what.to_string())
    }

    pub fn denied(what: impl fmt::Display, hint: impl Into<String>) -> Self {
        Error::Denied {
            what: what.to_string(),
            hint: hint.into(),
        }
    }

    pub fn os(detail: impl fmt::Display) -> Self {
        Error::Os(detail.to_string())
    }

    /// True when the cause is simply "this is not here any more".
    pub fn is_missing(&self) -> bool {
        matches!(self, Error::Missing(_))
    }

    pub fn is_denied(&self) -> bool {
        matches!(self, Error::Denied { .. })
    }
}
