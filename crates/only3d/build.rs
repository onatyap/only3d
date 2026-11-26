//! Windows: give only3d.exe and only3d-app.exe the app icon and version details (product name,
//! version, copyright). Code signing checks them: SignPath signs only files whose product name
//! is "only3d" and whose version is the release's.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../packaging/icons/only3d.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = env!("CARGO_PKG_VERSION");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../packaging/icons/only3d.ico")
        .set("ProductName", "only3d")
        .set("FileDescription", "only3d")
        .set("CompanyName", "only3d")
        .set("LegalCopyright", "AGPL-3.0-or-later")
        .set("ProductVersion", version)
        .set("FileVersion", version);
    if let Err(e) = res.compile() {
        panic!("Windows resources (icon and version details): {e}");
    }
}
