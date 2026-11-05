fn main() {
    println!("cargo:rerun-if-changed=../../assets/brand/ramwarden.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("../../assets/brand/ramwarden.ico")
            .set("ProductName", "RamWarden")
            .set("FileDescription", "RamWarden")
            .set("OriginalFilename", "ramwarden-tray.exe")
            .compile()
            .expect("compile the Windows product icon and version metadata");
    }
}
