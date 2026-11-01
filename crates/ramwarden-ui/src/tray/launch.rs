//! Starting the daemon when nothing answers on its port.
//!
//! The install puts the daemon in a logon task, but that task can be missing
//! (running from `cargo run`, a build that was never installed) or can have
//! given up: Task Scheduler restarts it three times and then stops. Either way
//! the window used to sit on "Waiting for the daemon…" forever, because nothing
//! on this side ever tried to start it.
//!
//! The launcher starts `ramwarden-daemon.exe` from beside this executable, the
//! same way the logon task does: under `conhost --headless`, so it has no window
//! but still receives the close/logoff/shutdown events it uses to restore Tidy
//! hints. Two launches cannot produce two daemons: the second one fails to bind
//! `:7823` and exits, and its log says so.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// How long to wait between launches. Longer than the daemon's own start-up,
/// so a daemon that is coming up is not started a second time.
const RETRY: Duration = Duration::from_secs(30);

pub struct DaemonLauncher {
    /// Grace before the first launch, so a logon task starting alongside the
    /// tray (or the tray's own launch, for the window) wins the race instead
    /// of tripping over a second copy.
    grace: Duration,
    exe: Option<PathBuf>,
    log: PathBuf,
    first_failure: Option<Instant>,
    last_launch: Option<Instant>,
    child: Option<Child>,
}

impl DaemonLauncher {
    pub fn new(grace: Duration) -> Self {
        let exe = std::env::current_exe()
            .ok()
            .map(|p| p.with_file_name("ramwarden-daemon.exe"))
            .filter(|p| p.exists());
        DaemonLauncher {
            grace,
            exe,
            log: Self::log_path(),
            first_failure: None,
            last_launch: None,
            child: None,
        }
    }

    /// Where the daemon writes its log: the same file the logon task uses.
    pub fn log_path() -> PathBuf {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("ramwarden")
            .join("daemon.log")
    }

    /// The daemon answered: forget the failure streak.
    pub fn connected(&mut self) {
        self.first_failure = None;
    }

    /// The daemon did not answer. Start it if it is time to, and describe
    /// what this side is doing about it, for the window to show.
    pub fn unreachable(&mut self) -> String {
        let now = Instant::now();
        let since = *self.first_failure.get_or_insert(now);
        let Some(exe) = self.exe.clone() else {
            return "ramwarden-daemon.exe is not installed beside this app, so it cannot be started from here. \
                    Run scripts\\install.ps1, or start it with `cargo run -p ramwarden-daemon`."
                .into();
        };
        if let Some(c) = &mut self.child
            && let Ok(Some(status)) = c.try_wait()
        {
            self.child = None;
            return format!(
                "ramwarden-daemon exited ({status}) — see {} for why.",
                self.log.display()
            );
        }
        if now.duration_since(since) < self.grace {
            return "Waiting for the daemon to start…".into();
        }
        if self.child.is_some() || self.last_launch.is_some_and(|t| now.duration_since(t) < RETRY) {
            return format!("Started ramwarden-daemon; waiting for it to answer (log: {}).", self.log.display());
        }
        self.last_launch = Some(now);
        match self.spawn(&exe) {
            Ok(c) => {
                tracing::info!("started {} (pid {})", exe.display(), c.id());
                self.child = Some(c);
                format!("The daemon was not running; started it (log: {}).", self.log.display())
            }
            Err(e) => {
                tracing::error!("cannot start {}: {e}", exe.display());
                format!("Cannot start {}: {e}", exe.display())
            }
        }
    }

    fn spawn(&self, exe: &PathBuf) -> std::io::Result<Child> {
        use std::os::windows::process::CommandExt;
        // No console window for either process.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let workdir = std::env::var_os("APPDATA")
            .map(|p| PathBuf::from(p).join("ramwarden"))
            .filter(|p| p.is_dir())
            .or_else(|| exe.parent().map(PathBuf::from))
            .unwrap_or_default();
        let conhost = std::env::var_os("SystemRoot")
            .map(|r| PathBuf::from(r).join("System32").join("conhost.exe"))
            .filter(|p| p.exists());
        let mut cmd = match conhost {
            Some(conhost) => {
                let mut c = Command::new(conhost);
                c.arg("--headless").arg(exe);
                c
            }
            None => Command::new(exe),
        };
        cmd.arg("--log-file")
            .arg(&self.log)
            .current_dir(workdir)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
    }
}
