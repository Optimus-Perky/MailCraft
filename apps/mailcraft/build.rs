//! Windows only: embed the app icon and version info into `MailCraft.exe`, so it shows in
//! Explorer, the taskbar, the Start menu and Alt-Tab.
//!
//! The version is MAILCRAFT_VERSION when the release build sets it (0.3.<commit count>, as
//! the installer is numbered), else the crate's. It must rise with every build: Windows
//! Installer only replaces a program file with a newer version of it.
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile still links, unless `MAILCRAFT_REQUIRE_WINRES=1` makes it an error.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/mailcraft.ico");
    println!("cargo:rerun-if-env-changed=MAILCRAFT_REQUIRE_WINRES");
    println!("cargo:rerun-if-env-changed=MAILCRAFT_VERSION");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = std::env::var("MAILCRAFT_VERSION").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let parts: Vec<u64> = version.split('.').map(|p| p.parse().unwrap_or(0)).chain(std::iter::repeat(0)).take(4).collect();
    let packed = parts.iter().fold(0u64, |acc, p| (acc << 16) | (p & 0xFFFF));
    let dotted = parts.iter().map(u64::to_string).collect::<Vec<_>>().join(".");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/app-icon/mailcraft.ico")
        .set("ProductName", "MailCraft")
        .set("FileDescription", "MailCraft")
        .set("FileVersion", &dotted)
        .set("ProductVersion", &version)
        .set("LegalCopyright", "Copyright (c) the MailCraft authors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "MailCraft.exe")
        .set("InternalName", "MailCraft")
        .set_version_info(winresource::VersionInfo::FILEVERSION, packed)
        .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
    if let Err(e) = res.compile() {
        if std::env::var_os("MAILCRAFT_REQUIRE_WINRES").is_some() {
            eprintln!("embedding Windows resources failed: {e}");
            std::process::exit(1);
        }
        println!("cargo:warning=mailcraft.exe built without icon/version resources: {e}");
    }
}
