//! Embeds the app icon, version information and application manifest into Pods4.exe.

fn main() {
    println!("cargo:rerun-if-changed=assets/pods4.ico");
    println!("cargo:rerun-if-changed=assets/pods4.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/pods4.ico")
        .set_manifest_file("assets/pods4.manifest")
        .set("ProductName", "Pods4")
        .set("FileDescription", "Pods4 - Galaxy Buds controls")
        .set("InternalName", "pods4")
        .set("OriginalFilename", "Pods4.exe")
        .set("CompanyName", "Pods4")
        .set("LegalCopyright", "MIT License");
    if let Err(e) = res.compile() {
        // Without a resource compiler the exe still builds, just without icon/version info.
        println!("cargo:warning=Could not embed Windows resources: {e}");
    }
}
