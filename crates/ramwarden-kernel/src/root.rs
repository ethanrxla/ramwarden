use std::path::{Path, PathBuf};

/// Where the kernel interfaces live.
///
/// Every reader and writer in this crate goes through a `Root` instead of naming
/// `/proc` and `/sys` directly. In production the root is `/`; in tests it is a
/// tempdir holding a synthetic kernel tree. That is the whole reason the
/// remediation ladder can be unit-tested — otherwise the only way to exercise
/// "what happens at 25% memory pressure" would be to put a real machine there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root(PathBuf);

impl Root {
    /// The real kernel.
    pub fn system() -> Self {
        Root(PathBuf::from("/"))
    }

    /// A synthetic tree, for tests and for inspecting a captured snapshot.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Root(path.into())
    }

    /// Resolve a root-relative path. `rel` must not begin with a separator —
    /// `Path::join` would discard the root and silently read the live kernel,
    /// which in a test looks like a pass for the wrong reason.
    pub fn join(&self, rel: impl AsRef<Path>) -> PathBuf {
        let rel = rel.as_ref();
        debug_assert!(
            rel.is_relative(),
            "Root::join needs a relative path, got {rel:?}"
        );
        self.0.join(rel)
    }

    pub fn proc_pid(&self, pid: i32, file: &str) -> PathBuf {
        self.join(format!("proc/{pid}/{file}"))
    }

    pub fn cgroup_fs(&self) -> PathBuf {
        self.join("sys/fs/cgroup")
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl Default for Root {
    fn default() -> Self {
        Root::system()
    }
}
