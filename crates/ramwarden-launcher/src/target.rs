//! Which build this machine needs, and which installer formats it can run.

use crate::manifest::Format;

/// An OS and CPU pair, spelt the way release assets are named:
/// `windows-x86_64`, `macos-aarch64`, `linux-x86_64`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub os: &'static str,
    pub arch: &'static str,
    /// Formats this install can apply, best first.
    pub formats: Vec<Format>,
}

impl Target {
    /// The machine this binary runs on.
    ///
    /// On Linux the format follows how RamWarden was installed: a launcher
    /// under `/usr` came from the `.deb` and is updated by one; anywhere else
    /// (`~/.local/bin`) came from the tarball. Switching a user between the two
    /// would leave two copies fighting over port 7823.
    pub fn current() -> Target {
        let os = std::env::consts::OS;
        let formats = match os {
            "windows" => vec![Format::Exe],
            "macos" => vec![Format::TarGz],
            "linux" => {
                let packaged = std::env::current_exe()
                    .map(|p| p.starts_with("/usr"))
                    .unwrap_or(false);
                if packaged {
                    vec![Format::Deb]
                } else {
                    vec![Format::TarGz]
                }
            }
            _ => Vec::new(),
        };
        Target {
            os,
            arch: std::env::consts::ARCH,
            formats,
        }
    }

    /// The asset-name form, e.g. `linux-x86_64`.
    pub fn triple(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_target_has_a_format_on_supported_platforms() {
        let t = Target::current();
        assert!(t.triple().contains('-'));
        if ["windows", "macos", "linux"].contains(&t.os) {
            assert!(!t.formats.is_empty());
        }
    }
}
