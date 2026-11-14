//! Embeds a Windows version resource in `nucleos-core.exe`, so Task Manager's Processes tab shows
//! "NucleOS Core" instead of the bare file name. A no-op on every other target.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set("FileDescription", "NucleOS Core")
        .set("ProductName", "NucleOS")
        .set("CompanyName", "NucleOS")
        .set("OriginalFilename", "nucleos-core.exe")
        .set("InternalName", "nucleos-core");
    // FileVersion/ProductVersion default to CARGO_PKG_VERSION. A missing resource compiler (no
    // Windows SDK) only costs the display name, so it warns instead of failing the build.
    if let Err(err) = res.compile() {
        println!("cargo:warning=version resource not embedded: {err}");
    }
}
