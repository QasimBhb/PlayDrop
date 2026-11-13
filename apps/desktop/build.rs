//! Compiles the Slint UI in `ui/` into Rust at build time, and on Windows embeds the icon.

fn main() {
    slint_build::compile("ui/app.slint").expect("compile Slint UI");
    windows_resources("PlayDrop");
}

/// On Windows, gives the program its icon and its name in Explorer, the Start menu, the
/// taskbar and Task Manager. Without this resource Windows shows a blank icon and the file
/// name. The build script runs on the build machine, so this is compiled only there, and
/// does nothing when a Windows machine builds for another system.
#[cfg(windows)]
fn windows_resources(description: &str) {
    const ICON: &str = "../../packaging/windows/playdrop.ico";
    println!("cargo:rerun-if-changed={ICON}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ICON);
    res.set("FileDescription", description);
    res.set("ProductName", "PlayDrop");
    res.set("CompanyName", "PlayDrop contributors");
    res.set("LegalCopyright", "GPL-3.0-or-later");
    res.compile()
        .expect("compile the Windows icon and version resource");
}

#[cfg(not(windows))]
fn windows_resources(_description: &str) {}
