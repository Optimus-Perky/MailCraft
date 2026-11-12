//! Windows only: embed the app icon and version info into `MailCraft.exe`, so it shows in
//! Explorer, the taskbar, the Start menu and Alt-Tab.
//!
//! The version is MAILCRAFT_VERSION when the release build sets it (0.3.<commit count>, as
//! the installer is numbered), else the crate's. It must rise with every build: Windows
//! Installer only replaces a program file with a newer version of it.
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile still links, unless `MAILCRAFT_REQUIRE_WINRES=1` makes it an error.
//!
//! On every target: the built-in Google sign-in. A release build sets MAILCRAFT_OAUTH_FILE to
//! an oauth.json (the same shape as the one in MailCraft's settings folder) kept outside the
//! source tree; its "google" registration is compiled in, so people can sign in with Google
//! without a registration of their own. Google's desktop-app "secret" ships inside every copy
//! of a desktop app and Google does not treat it as confidential, but it still never goes in
//! the repository. Without the variable the build has no built-in registration and asks for
//! the person's own, as before.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/mailcraft.ico");
    println!("cargo:rerun-if-env-changed=MAILCRAFT_REQUIRE_WINRES");
    println!("cargo:rerun-if-env-changed=MAILCRAFT_VERSION");
    builtin_google();
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

/// Compile in the "google" registration from MAILCRAFT_OAUTH_FILE, if set. A file that is set
/// but unusable stops the build, so a release never ships without sign-in by accident. The
/// values are never printed, not even in an error.
fn builtin_google() {
    println!("cargo:rerun-if-env-changed=MAILCRAFT_OAUTH_FILE");
    let Some(path) = std::env::var_os("MAILCRAFT_OAUTH_FILE").filter(|p| !p.is_empty()) else { return };
    println!("cargo:rerun-if-changed={}", std::path::Path::new(&path).display());
    let fail = |why: &str| -> ! {
        eprintln!("MAILCRAFT_OAUTH_FILE: {why}");
        std::process::exit(1);
    };
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| fail("cannot read the file"));
    // Notepad and PowerShell may add a byte-order mark.
    let json: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_else(|_| fail("not valid JSON"));
    let google = json.get("google").unwrap_or_else(|| fail("no \"google\" registration in it"));
    let field = |k: &str| google.get(k).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).unwrap_or_default();
    let (id, secret) = (field("client_id"), field("client_secret"));
    let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if !plain(&id) || !id.ends_with(".apps.googleusercontent.com") {
        fail("the google client_id is not a Google client ID (...apps.googleusercontent.com)");
    }
    if !plain(&secret) {
        fail("the google client_secret is missing or has unexpected characters");
    }
    println!("cargo:rustc-env=MAILCRAFT_BUILTIN_GOOGLE_ID={id}");
    println!("cargo:rustc-env=MAILCRAFT_BUILTIN_GOOGLE_SECRET={secret}");
}
