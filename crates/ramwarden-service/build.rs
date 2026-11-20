//! Windows only: give the SYSTEM service the ram icon and a readable name.
//!
//! Task Manager, Startup apps, the firewall prompt and Services show a
//! process by its embedded icon and FileDescription. Without them ramwarden-service.exe
//! is a blank program icon and a file name, the Windows twin of the generic
//! "exec" row macOS showed in Full Disk Access before the daemon had a bundle.
fn main() {
    println!("cargo:rerun-if-changed=../../assets/brand/ramwarden.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("../../assets/brand/ramwarden.ico")
            .set("ProductName", "RamWarden")
            .set("FileDescription", "RamWarden Service")
            .set("OriginalFilename", "ramwarden-service.exe")
            .compile()
            .expect("compile the Windows icon and version metadata");
    }
}
