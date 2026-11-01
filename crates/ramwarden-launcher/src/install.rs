//! Running a verified installer, the platform's way.
//!
//! [`Installer::plan`] is pure, so the command each format becomes is tested
//! on every OS; [`Installer::run`] executes it.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::manifest::Format;
use crate::{Error, Result};

/// What running the installer achieved. Reported as is: a started installer
/// is not an installed one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ran {
    /// The installer exited 0.
    Finished,
    /// The installer was started and the launcher must not wait: it is about to
    /// replace the launcher's own executable (Windows locks a running exe).
    Started,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub wait: bool,
}

pub struct Installer<'a> {
    pub format: Format,
    /// The downloaded, hash-checked file.
    pub file: &'a Path,
    /// Where a tarball is unpacked.
    pub unpack_dir: &'a Path,
    /// The installer replaces the running launcher (the RAM warden does).
    pub replaces_self: bool,
    pub is_root: bool,
}

impl Installer<'_> {
    pub fn plan(&self) -> Plan {
        match self.format {
            Format::Exe => {
                // Inno Setup's silent flags; `-Verb RunAs` raises the UAC prompt
                // that the service install needs. With `-Wait -PassThru` the exit
                // code (and a declined prompt, which throws) comes back to us.
                let mut cmd = format!(
                    "$p = Start-Process -FilePath {} -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART' -Verb RunAs",
                    ps_quote(self.file)
                );
                if !self.replaces_self {
                    cmd.push_str(" -Wait -PassThru; exit $p.ExitCode");
                }
                Plan {
                    program: "powershell.exe".into(),
                    args: vec![
                        "-NoProfile".into(),
                        "-NonInteractive".into(),
                        "-Command".into(),
                        cmd,
                    ],
                    cwd: None,
                    wait: true,
                }
            }
            Format::Deb => {
                // apt rather than dpkg so dependencies resolve. pkexec shows
                // the desktop's own password prompt; root needs neither.
                let mut args = vec![
                    "apt-get".to_string(),
                    "install".into(),
                    "-y".into(),
                    self.file.display().to_string(),
                ];
                let program = if self.is_root {
                    args.remove(0)
                } else {
                    "pkexec".to_string()
                };
                Plan {
                    program,
                    args,
                    cwd: None,
                    wait: true,
                }
            }
            Format::TarGz => Plan {
                program: "/bin/sh".into(),
                args: vec!["install.sh".into()],
                cwd: Some(self.unpack_dir.to_path_buf()),
                wait: true,
            },
        }
    }

    pub fn run(&self) -> Result<Ran> {
        if self.format == Format::TarGz {
            unpack(self.file, self.unpack_dir)?;
            if !self.unpack_dir.join("install.sh").is_file() {
                return Err(Error::Install("the archive has no install.sh".into()));
            }
        }
        let plan = self.plan();
        let mut cmd = Command::new(&plan.program);
        cmd.args(&plan.args);
        if let Some(dir) = &plan.cwd {
            cmd.current_dir(dir);
        }
        let status = cmd
            .status()
            .map_err(|e| Error::Install(format!("{}: {e}", plan.program)))?;
        if !status.success() {
            return Err(Error::Install(format!(
                "{} exited with {status} (a declined password prompt also ends here)",
                plan.program
            )));
        }
        Ok(if self.format == Format::Exe && self.replaces_self {
            Ran::Started
        } else {
            Ran::Finished
        })
    }
}

/// PowerShell single-quoted literal: `'` doubles, nothing else is special.
fn ps_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "''"))
}

/// Unpack a `.tar.gz` into an empty `dest`, refusing any entry that is
/// absolute, climbs with `..`, or is a link: an archive must not be able to
/// write outside the directory it is given, even a correctly signed one.
pub fn unpack(archive: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::create_dir_all(dest)?;
    let gz = flate2::read::GzDecoder::new(std::fs::File::open(archive)?);
    let mut tar = tar::Archive::new(gz);
    tar.set_preserve_permissions(true);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let kind = entry.header().entry_type();
        let escapes = path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir));
        if escapes || kind.is_symlink() || kind.is_hard_link() {
            return Err(Error::Install(format!(
                "archive entry {} is not a plain file inside the archive",
                path.display()
            )));
        }
        entry.unpack_in(dest)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installer<'a>(format: Format, file: &'a Path, dir: &'a Path, replaces_self: bool, is_root: bool) -> Installer<'a> {
        Installer {
            format,
            file,
            unpack_dir: dir,
            replaces_self,
            is_root,
        }
    }

    #[test]
    fn windows_ram_update_does_not_wait_but_addons_do() {
        let f = Path::new(r"C:\Users\o'neil\AppData\Local\ramwarden\launcher\staging\setup.exe");
        let d = Path::new("x");
        let p = installer(Format::Exe, f, d, true, false).plan();
        let cmd = p.args.last().unwrap();
        assert!(cmd.contains("-Verb RunAs") && cmd.contains("/VERYSILENT"));
        assert!(cmd.contains("'C:\\Users\\o''neil\\"), "quote doubled: {cmd}");
        assert!(!cmd.contains("-Wait"));
        let p = installer(Format::Exe, f, d, false, false).plan();
        assert!(p.args.last().unwrap().ends_with("exit $p.ExitCode"));
    }

    #[test]
    fn deb_uses_pkexec_unless_root() {
        let f = Path::new("/tmp/r.deb");
        let p = installer(Format::Deb, f, f, true, false).plan();
        assert_eq!(p.program, "pkexec");
        assert_eq!(p.args, ["apt-get", "install", "-y", "/tmp/r.deb"]);
        let p = installer(Format::Deb, f, f, true, true).plan();
        assert_eq!(p.program, "apt-get");
        assert_eq!(p.args, ["install", "-y", "/tmp/r.deb"]);
    }

    fn tarball(dir: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join("a.tar.gz");
        let gz = flate2::write::GzEncoder::new(std::fs::File::create(&path).unwrap(), flate2::Compression::fast());
        let mut b = tar::Builder::new(gz);
        for (name, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            // Bypass `append_data`'s own path checks so the hostile cases can
            // be written at all.
            let bytes = name.as_bytes();
            h.as_old_mut().name[..bytes.len()].copy_from_slice(bytes);
            h.set_cksum();
            b.append(&h, *data).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn unpacks_plain_files() {
        let t = tempfile::tempdir().unwrap();
        let a = tarball(t.path(), &[("install.sh", b"echo hi\n"), ("bin/ramwarden-daemon", b"\x7fELF")]);
        let out = t.path().join("out");
        unpack(&a, &out).unwrap();
        assert!(out.join("install.sh").is_file());
        assert!(out.join("bin/ramwarden-daemon").is_file());
    }

    /// The release scripts build with `tar -C stage -czf out .`, whose entries
    /// start with `./` and include `./` itself.
    #[cfg(unix)]
    #[test]
    fn unpacks_what_the_release_scripts_build() {
        let t = tempfile::tempdir().unwrap();
        let stage = t.path().join("stage");
        std::fs::create_dir_all(stage.join("bin")).unwrap();
        std::fs::write(stage.join("install.sh"), "exit 0\n").unwrap();
        std::fs::write(stage.join("bin/ramwarden-daemon"), "x").unwrap();
        let a = t.path().join("r.tar.gz");
        let ok = Command::new("tar")
            .arg("-C")
            .arg(&stage)
            .arg("-czf")
            .arg(&a)
            .arg(".")
            .status()
            .unwrap();
        assert!(ok.success());
        let out = t.path().join("out");
        unpack(&a, &out).unwrap();
        assert!(out.join("install.sh").is_file());
        assert!(out.join("bin/ramwarden-daemon").is_file());
    }

    #[test]
    fn refuses_entries_that_climb_out() {
        let t = tempfile::tempdir().unwrap();
        let a = tarball(t.path(), &[("../evil", b"x")]);
        assert!(unpack(&a, &t.path().join("out")).is_err());
        assert!(!t.path().join("evil").exists());
    }

    #[cfg(unix)]
    #[test]
    fn runs_install_sh_from_the_unpacked_tree() {
        let t = tempfile::tempdir().unwrap();
        let a = tarball(t.path(), &[("install.sh", b"touch ran\n")]);
        let out = t.path().join("out");
        let ran = installer(Format::TarGz, &a, &out, true, false).run().unwrap();
        assert_eq!(ran, Ran::Finished);
        assert!(out.join("ran").exists());

        let bad = tarball(t.path(), &[("install.sh", b"exit 3\n")]);
        assert!(installer(Format::TarGz, &bad, &out, true, false).run().is_err());
    }
}
