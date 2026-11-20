//! Windows: the SYSTEM service's ram icon and name in Services and Task Manager.
include!("../build-support/winres.rs");

fn main() {
    ram_resources("RamWarden Service", "ramwarden-service.exe");
}
