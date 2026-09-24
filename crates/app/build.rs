//! Windows: embeds the icon (Explorer, the taskbar, the Start menu, the installer) and the
//! version details a program's Properties show. Elsewhere, nothing.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/logo.ico");
    #[cfg(windows)]
    {
        let version = env!("CARGO_PKG_VERSION");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/logo.ico")
            .set("ProductName", "OpenAtelier")
            .set("FileDescription", "OpenAtelier video editor")
            .set("CompanyName", "OpenAtelier contributors")
            .set("LegalCopyright", "AGPL-3.0-or-later")
            .set("OriginalFilename", "OpenAtelier.exe")
            .set("ProductVersion", version)
            .set("FileVersion", version);
        // A missing resource compiler shouldn't stop a build: the program just has no icon.
        if let Err(e) = res.compile() {
            println!("cargo:warning=no icon embedded: {e}");
        }
    }
}
