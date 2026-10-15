//! The platform for the machine this binary is running on.
//!
//! The one place that chooses between mechanism crates, so the daemon, the
//! examples and the UI never repeat the `cfg` selection. Policy elsewhere in
//! this crate only ever sees `ramwarden_platform` traits.

use ramwarden_platform::Platform;

/// The real platform: Linux (`/proc`, cgroup v2, PSI) or Windows (NT APIs).
#[cfg(target_os = "linux")]
pub fn platform() -> Platform {
    ramwarden_kernel::platform::platform(ramwarden_kernel::Root::system())
}

/// The real platform: macOS (libproc, sysctl, mach).
#[cfg(target_os = "macos")]
pub fn platform() -> Platform {
    ramwarden_mac::platform()
}

/// The real platform: Windows (NT APIs).
#[cfg(windows)]
pub fn platform() -> Platform {
    ramwarden_nt::platform::platform()
}
