//! Windows: the daemon's ram icon and name, the twin of the macOS bundle that
//! makes Full Disk Access list it as RamWarden (`macos/daemon/bundle.sh`).
include!("../build-support/winres.rs");

fn main() {
    ram_resources("RamWarden Warden", "ramwarden-daemon.exe");
}
