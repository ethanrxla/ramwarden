use std::io;
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// A kernel file existed but did not hold what its format promises. Worth
    /// distinguishing from Io: a parse failure means the kernel interface changed
    /// under us, which is a bug to fix rather than a condition to handle.
    #[error("{path}: could not parse: {detail}")]
    Parse { path: PathBuf, detail: String },

    /// The kernel on this machine does not expose the interface at all. Callers
    /// are expected to degrade rather than fail — this box has no
    /// `zram0/recomp_algorithm`, for instance, and that is not an error.
    #[error("{0} is not available on this kernel")]
    Unsupported(&'static str),

    /// The interface exists but this process may not use it. Separate from Io so
    /// the remediation ladder can log "no CAP_SYS_NICE, using cgroup reclaim
    /// instead" rather than treating a permission boundary as a failure.
    #[error("{path}: permission denied ({hint})")]
    Denied {
        path: PathBuf,
        hint: &'static str,
    },
}

impl Error {
    pub(crate) fn io(path: impl AsRef<Path>, source: io::Error) -> Self {
        let path = path.as_ref().to_path_buf();
        if source.kind() == io::ErrorKind::PermissionDenied {
            return Error::Denied {
                path,
                hint: "check ownership of the cgroup scope or the process",
            };
        }
        Error::Io { path, source }
    }

    pub(crate) fn parse(path: impl AsRef<Path>, detail: impl Into<String>) -> Self {
        Error::Parse {
            path: path.as_ref().to_path_buf(),
            detail: detail.into(),
        }
    }

    /// True when the underlying cause is simply "this file is not here".
    pub fn is_missing(&self) -> bool {
        matches!(self, Error::Io { source, .. } if source.kind() == io::ErrorKind::NotFound)
    }
}
