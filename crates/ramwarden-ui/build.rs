fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=../../assets/brand/ramwarden.ico");
        let mut resources = winresource::WindowsResource::new();
        resources.set_icon("../../assets/brand/ramwarden.ico")
            .set("ProductName", "RamWarden")
            .set("FileDescription", "RamWarden desktop companion")
            .set("CompanyName", "RamWarden");
        resources.compile().expect("embed Windows product icon and version");
    }
}
