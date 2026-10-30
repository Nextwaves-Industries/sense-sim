// Embeds the app icon and version info into the Windows executable.
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") && std::path::Path::new("assets/icon.ico").exists() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Sense Sim");
        res.set("FileDescription", "Sense Sim");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}
