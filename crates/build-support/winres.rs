// Shared by the build scripts of every Windows executable RamWarden ships
// (tray, daemon, service), pulled in with `include!` because a build script
// cannot depend on a workspace crate without building it for the host first.
//
// Task Manager, Startup apps, the firewall prompt and Services show a process
// by its embedded icon and FileDescription; without them an exe is a blank
// program icon and a file name.

/// Embed the ram icon and version metadata into this package's binaries.
#[allow(unused_variables)]
fn ram_resources(description: &str, original_filename: &str) {
    println!("cargo:rerun-if-changed=../../assets/brand/ramwarden.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("../../assets/brand/ramwarden.ico")
            .set("ProductName", "RamWarden")
            .set("FileDescription", description)
            .set("OriginalFilename", original_filename)
            .compile()
            .expect("compile the Windows icon and version metadata");
    }
}
