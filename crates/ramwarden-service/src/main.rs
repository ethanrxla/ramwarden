//! `ramwarden-service`: the Windows counterpart of the Linux setcap helper.
//!
//! Runs as LocalSystem so the unprivileged daemon never has to. It answers a
//! closed set of requests over `\\.\pipe\ramwarden` (see
//! `ramwarden_nt::pipe::Request`) and does nothing on its own initiative.
//!
//! ```text
//! ramwarden-service install     # elevated; binary must be under Program Files
//! ramwarden-service uninstall   # elevated
//! ramwarden-service console     # run in the foreground, for debugging
//! ```
//!
//! # Why the binary must live under Program Files
//!
//! A service runs whatever file its registration points at, as SYSTEM. If that
//! file sits somewhere the user can write — a build directory, Downloads —
//! anything running as the user can replace it and become SYSTEM at the next
//! start. `install` therefore refuses any path outside `%ProgramFiles%`, which
//! only administrators can write.

#[cfg(windows)]
mod handler;
#[cfg(windows)]
mod secure;
#[cfg(windows)]
mod server;

#[cfg(windows)]
mod windows_main {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::Duration;

    use tokio::sync::watch;
    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};

    pub const NAME: &str = "RamWarden";
    const DISPLAY: &str = "RamWarden memory service";
    const DESCRIPTION: &str = "Privileged helper for RamWarden: trims elevated apps in your session and \
        applies the reversible privacy/background profiles. Accepts requests only from interactive users.";

    pub fn main() {
        match std::env::args().nth(1).as_deref() {
            Some("install") => exit(install()),
            Some("uninstall") => exit(uninstall()),
            Some("console") => {
                init_log(false);
                let (_tx, rx) = watch::channel(false);
                exit(runtime().block_on(crate::server::serve(rx)).map_err(|e| e.to_string()));
            }
            _ => {
                if let Err(e) = service_dispatcher::start(NAME, ffi_service_main) {
                    eprintln!(
                        "not started by the service manager ({e}).\n\
                         usage: ramwarden-service install | uninstall | console"
                    );
                    std::process::exit(2);
                }
            }
        }
    }

    fn exit(r: Result<(), String>) {
        if let Err(e) = r {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime")
    }

    fn log_dir() -> PathBuf {
        std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join("RamWarden")
    }

    fn init_log(to_file: bool) {
        let filter = tracing_subscriber::EnvFilter::try_from_env("RAMWARDEN_LOG")
            .unwrap_or_else(|_| "info".into());
        if to_file {
            let _ = std::fs::create_dir_all(log_dir());
            if let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_dir().join("service.log"))
            {
                tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_ansi(false)
                    .with_writer(std::sync::Mutex::new(file))
                    .init();
                return;
            }
        }
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_args: Vec<OsString>) {
        // Before anything is written there: see `secure` for why.
        let locked = crate::secure::lock_down(&log_dir());
        init_log(true);
        if let Err(e) = locked {
            tracing::error!("cannot secure {}: {e} — refusing to run", log_dir().display());
            return;
        }
        if let Err(e) = run_service() {
            tracing::error!("service failed: {e}");
        }
    }

    fn status(state: ServiceState, accept: ServiceControlAccept) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        }
    }

    fn run_service() -> windows_service::Result<()> {
        let (tx, rx) = watch::channel(false);
        let handle = service_control_handler::register(NAME, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = tx.send(true);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })?;
        handle.set_service_status(status(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        ))?;
        if let Err(e) = runtime().block_on(crate::server::serve(rx)) {
            tracing::error!("pipe server: {e}");
        }
        handle.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()))?;
        Ok(())
    }

    /// The executable must be somewhere only administrators can write.
    fn check_location(exe: &std::path::Path) -> Result<(), String> {
        let pf = std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .ok_or("%ProgramFiles% is not set")?;
        let exe = exe.canonicalize().map_err(|e| e.to_string())?;
        let pf = pf.canonicalize().map_err(|e| e.to_string())?;
        if !exe.starts_with(&pf) {
            return Err(format!(
                "refusing to register {} as a SYSTEM service: it is outside {}, so a normal user \
                 could replace it. Copy it under Program Files first (scripts/install.ps1 does this).",
                exe.display(),
                pf.display()
            ));
        }
        Ok(())
    }

    fn install() -> Result<(), String> {
        if !ramwarden_nt::privilege::is_elevated() {
            return Err("install needs an elevated (administrator) terminal".into());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        check_location(&exe)?;
        let manager = ServiceManager::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )
        .map_err(|e| e.to_string())?;
        let info = ServiceInfo {
            name: NAME.into(),
            display_name: DISPLAY.into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe,
            launch_arguments: vec![],
            dependencies: vec![],
            account_name: None, // LocalSystem
            account_password: None,
        };
        let service = manager
            .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
            .map_err(|e| format!("create service: {e}"))?;
        let _ = service.set_description(DESCRIPTION);
        service.start::<&str>(&[]).map_err(|e| format!("start: {e}"))?;
        println!("installed and started {NAME}");
        Ok(())
    }

    fn uninstall() -> Result<(), String> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
            .map_err(|e| e.to_string())?;
        let service = manager
            .open_service(NAME, ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS)
            .map_err(|e| format!("open {NAME}: {e}"))?;
        let _ = service.stop();
        service.delete().map_err(|e| format!("delete: {e}"))?;
        println!("removed {NAME}");
        Ok(())
    }
}

#[cfg(windows)]
fn main() {
    windows_main::main();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ramwarden-service is Windows-only; on Linux, use ramwarden-helper");
}
