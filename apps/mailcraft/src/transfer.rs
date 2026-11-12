//! Moving MailCraft to another PC (File > Export settings / Import settings): one file with the
//! accounts, settings, rules, categories, send-only addresses, calendar accounts, layout and the
//! saved passwords and sign-ins, all of it locked with a password the person chooses
//! (PBKDF2-SHA256, then AES-256-GCM): without the password nothing in it can be read. The mail
//! itself is not in it: the other PC fetches that.
//!
//! Importing replaces this PC's settings (after copying them to a backup folder), then
//! MailCraft starts again to use them.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use base64::Engine;
use egui::{Color32, RichText, vec2};
use ring::rand::SecureRandom;
use serde::{Deserialize, Serialize};
use wordcraft_ui_egui::theme::{Tokens, regular, semibold};

use crate::MailApp;

/// The settings files that travel (all in the config folder).
const FILES: &[&str] =
    &["accounts.json", "settings.json", "view.json", "senders.json", "categories.json", "rules.json", "folders.json", "calendars.json", "oauth.json"];

/// What belongs to one screen and not another: kept as they are on the PC importing.
const VIEW_LOCAL: &[&str] = &["window", "maximized"];

const FORMAT: &str = "mailcraft-settings";
const ITERATIONS: u32 = 600_000;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// The file as written: what it is, and the locked contents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Locked {
    pub format: String,
    pub version: u32,
    pub sealed: Sealed,
}

/// The contents, once unlocked.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bundle {
    pub exported: String,
    /// The PC it came from.
    pub from: String,
    pub files: BTreeMap<String, serde_json::Value>,
    /// Passwords and sign-ins, by where they are filed.
    pub secrets: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sealed {
    pub kdf: String,
    pub iterations: u32,
    pub salt: String,
    pub nonce: String,
    pub data: String,
}

fn key(passphrase: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut k = [0u8; 32];
    let n = NonZeroU32::new(iterations.max(1)).unwrap_or(NonZeroU32::MIN);
    ring::pbkdf2::derive(ring::pbkdf2::PBKDF2_HMAC_SHA256, n, salt, passphrase.as_bytes(), &mut k);
    k
}

fn seal(plain: &[u8], passphrase: &str, iterations: u32) -> Result<Sealed, String> {
    let rng = ring::rand::SystemRandom::new();
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 12];
    rng.fill(&mut salt).map_err(|_| "no random numbers available")?;
    rng.fill(&mut nonce).map_err(|_| "no random numbers available")?;
    let k = key(passphrase, &salt, iterations);
    let unbound = ring::aead::UnboundKey::new(&ring::aead::AES_256_GCM, &k).map_err(|_| "encryption key")?;
    let sealing = ring::aead::LessSafeKey::new(unbound);
    let mut buf = plain.to_vec();
    sealing
        .seal_in_place_append_tag(ring::aead::Nonce::assume_unique_for_key(nonce), ring::aead::Aad::from(FORMAT.as_bytes()), &mut buf)
        .map_err(|_| "encryption failed")?;
    Ok(Sealed { kdf: "pbkdf2-sha256".into(), iterations, salt: B64.encode(salt), nonce: B64.encode(nonce), data: B64.encode(buf) })
}

fn open(s: &Sealed, passphrase: &str) -> Result<Vec<u8>, String> {
    if s.kdf != "pbkdf2-sha256" {
        return Err(format!("the file is locked in a way this MailCraft does not know ({})", s.kdf));
    }
    let bad = || "the file is damaged".to_string();
    let salt = B64.decode(&s.salt).map_err(|_| bad())?;
    let nonce: [u8; 12] = B64.decode(&s.nonce).map_err(|_| bad())?.try_into().map_err(|_| bad())?;
    let mut buf = B64.decode(&s.data).map_err(|_| bad())?;
    let k = key(passphrase, &salt, s.iterations.min(10_000_000));
    let unbound = ring::aead::UnboundKey::new(&ring::aead::AES_256_GCM, &k).map_err(|_| "encryption key")?;
    let opening = ring::aead::LessSafeKey::new(unbound);
    let plain = opening
        .open_in_place(ring::aead::Nonce::assume_unique_for_key(nonce), ring::aead::Aad::from(FORMAT.as_bytes()), &mut buf)
        .map_err(|_| "That is not the password this file was exported with.".to_string())?;
    Ok(plain.to_vec())
}

fn this_pc() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "this PC".into())
}

/// Where each saved password or sign-in is filed: the accounts, the send-only addresses, and
/// the calendar accounts with their own.
fn secret_names() -> Vec<String> {
    let mut out: Vec<String> = crate::accounts::load().into_iter().map(|a| a.email).collect();
    out.extend(crate::accounts::load_senders().iter().map(|s| s.secret_name()));
    out.extend(crate::calendar::load_sources().iter().map(|s| s.secret_name()));
    out.sort();
    out.dedup();
    out
}

/// This PC's settings and saved passwords.
fn gather(dir: &Path) -> Bundle {
    let mut files = BTreeMap::new();
    for name in FILES {
        let Ok(bytes) = std::fs::read(dir.join(name)) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}')) else { continue };
        if *name == "view.json"
            && let Some(o) = v.as_object_mut()
        {
            for k in VIEW_LOCAL {
                o.remove(*k);
            }
        }
        files.insert(name.to_string(), v);
    }
    let mut secrets = BTreeMap::new();
    for n in secret_names() {
        if let Some(v) = crate::secrets::get(&n) {
            secrets.insert(n, v);
        }
    }
    let exported = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    Bundle { exported, from: this_pc(), files, secrets }
}

/// A bundle locked with a password, as the file's text.
fn lock(b: &Bundle, password: &str, iterations: u32) -> Result<Vec<u8>, String> {
    let plain = serde_json::to_vec(b).map_err(|e| e.to_string())?;
    let locked = Locked { format: FORMAT.into(), version: 2, sealed: seal(&plain, password, iterations)? };
    serde_json::to_vec_pretty(&locked).map_err(|e| e.to_string())
}

/// Opens a settings file with its password.
fn unlock(l: &Locked, password: &str) -> Result<Bundle, String> {
    serde_json::from_slice(&open(&l.sealed, password)?).map_err(|_| "the file is damaged".to_string())
}

pub fn read(path: &Path) -> Result<Locked, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if bytes.len() > 32 << 20 {
        return Err("that file is too big to be MailCraft settings".into());
    }
    let b: Locked = serde_json::from_slice(&bytes).map_err(|_| "that is not a MailCraft settings file".to_string())?;
    if b.format != FORMAT {
        return Err("that is not a MailCraft settings file".into());
    }
    if b.version > 2 {
        return Err("that file comes from a newer MailCraft: update this one first".into());
    }
    Ok(b)
}

/// What a bundle holds, for the person to see before importing it.
pub fn describe(b: &Bundle) -> Vec<String> {
    let count = |f: &str| b.files.get(f).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    let mut out = vec![];
    let accounts: Vec<String> = b
        .files
        .get("accounts.json")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.get("email").and_then(|e| e.as_str()).map(str::to_string)).collect())
        .unwrap_or_default();
    out.push(match accounts.len() {
        0 => "No email accounts".to_string(),
        _ => format!("Email accounts: {}", accounts.join(", ")),
    });
    for (f, one, many) in [
        ("senders.json", "send-only address", "send-only addresses"),
        ("calendars.json", "calendar account", "calendar accounts"),
        ("rules.json", "rule", "rules"),
        ("categories.json", "category", "categories"),
    ] {
        match count(f) {
            0 => {}
            1 => out.push(format!("1 {one}")),
            n => out.push(format!("{n} {many}")),
        }
    }
    if b.files.contains_key("settings.json") {
        out.push("Settings, signatures, templates and quick steps".into());
    }
    if b.files.contains_key("view.json") {
        out.push("Layout and theme".into());
    }
    if b.files.contains_key("oauth.json") {
        out.push("Google / Microsoft app registrations".into());
    }
    out.push(match b.secrets.len() {
        0 => "No saved passwords: you sign in again on this PC".into(),
        1 => "1 saved password or sign-in".into(),
        n => format!("{n} saved passwords and sign-ins"),
    });
    out
}

/// Puts an unlocked bundle's settings and passwords on this PC, after copying what is here now
/// to a backup folder. The backup folder.
fn apply(b: &Bundle, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let backup = dir.join(format!("backup-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    std::fs::create_dir_all(&backup).map_err(|e| format!("could not make the backup folder: {e}"))?;
    for name in FILES {
        let from = dir.join(name);
        if from.exists() {
            std::fs::copy(&from, backup.join(name)).map_err(|e| format!("could not back up {name}: {e}"))?;
        }
    }
    for (name, v) in &b.files {
        // Only the known files, by their own names (never a path from the file).
        if !FILES.contains(&name.as_str()) {
            continue;
        }
        let mut v = v.clone();
        if name == "view.json"
            && let (Some(o), Ok(here)) = (v.as_object_mut(), std::fs::read(dir.join(name)))
            && let Ok(serde_json::Value::Object(mine)) = serde_json::from_slice::<serde_json::Value>(&here)
        {
            for k in VIEW_LOCAL {
                if let Some(x) = mine.get(*k) {
                    o.insert(k.to_string(), x.clone());
                }
            }
        }
        let text = serde_json::to_vec_pretty(&v).map_err(|e| e.to_string())?;
        let tmp = dir.join(format!("{name}.importing"));
        std::fs::write(&tmp, text).map_err(|e| format!("could not write {name}: {e}"))?;
        std::fs::rename(&tmp, dir.join(name)).map_err(|e| format!("could not write {name}: {e}"))?;
    }
    for (n, s) in &b.secrets {
        crate::secrets::set(n, s);
    }
    Ok(backup)
}

#[derive(Default)]
pub struct Form {
    import: bool,
    pass: String,
    pass2: String,
    /// Import: the file chosen, and its contents once unlocked.
    locked: Option<Locked>,
    bundle: Option<Bundle>,
    error: Option<String>,
    done: Option<String>,
    restart: bool,
}

impl MailApp {
    pub(crate) fn export_settings(&mut self) {
        self.transfer = Some(Form::default());
    }

    pub(crate) fn import_settings(&mut self) {
        let Some(path) = test_file()
            .or_else(|| rfd::FileDialog::new().set_title("Import MailCraft settings").add_filter("MailCraft settings", &["mcsettings"]).pick_file())
        else {
            return;
        };
        let mut f = Form { import: true, ..Default::default() };
        match read(&path) {
            Ok(l) => f.locked = Some(l),
            Err(e) => f.error = Some(e),
        }
        self.transfer = Some(f);
    }

    pub(crate) fn transfer_dialog(&mut self, ctx: &egui::Context, t: &Tokens) {
        let Some(mut f) = self.transfer.take() else { return };
        let mut close = false;
        let mut go = false;
        let title = if f.import { "Import settings" } else { "Export settings" };
        egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 7.0);
            let note = |ui: &mut egui::Ui, s: &str| {
                ui.add(egui::Label::new(RichText::new(s).font(regular(12.0)).color(t.text_dim)).wrap());
            };
            let field = |ui: &mut egui::Ui, label: &str, s: &mut String, id: &str| {
                ui.horizontal(|ui| {
                    ui.add_sized([90.0, 22.0], egui::Label::new(label));
                    let r = ui.add_sized([300.0, 22.0], egui::TextEdit::singleline(s).id(egui::Id::new(id)).password(true));
                    if id.ends_with('1') && s.is_empty() && ui.memory(|m| m.focused().is_none()) {
                        r.request_focus();
                    }
                    r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                })
                .inner
            };
            let mut enter = false;
            if let Some(done) = &f.done {
                ui.add(egui::Label::new(RichText::new(done).color(t.text)).wrap());
                ui.add_space(6.0);
                let label = if f.restart { "Restart MailCraft" } else { "Close" };
                let b = egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(t.accent).min_size(vec2(90.0, 28.0));
                if ui.add(b).clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = true;
                }
                return;
            }
            let ready = if f.import {
                match (&f.locked, &f.bundle) {
                    (None, _) => false,
                    (Some(_), None) => {
                        note(ui, "This settings file is locked. Enter the password it was exported with.");
                        enter |= field(ui, "Password", &mut f.pass, "tr_p1");
                        !f.pass.is_empty()
                    }
                    (Some(_), Some(b)) => {
                        ui.label(RichText::new(format!("From {} on {}", b.from, b.exported)).font(semibold(13.5)).color(t.text));
                        for line in describe(b) {
                            ui.label(RichText::new(format!("\u{2022}  {line}")).font(regular(12.5)).color(t.text));
                        }
                        ui.add_space(4.0);
                        note(
                            ui,
                            "This replaces the accounts and settings on this PC. What is here now is copied to a backup folder first. The mail is fetched again from the servers.",
                        );
                        enter |= ui.input(|i| i.key_pressed(egui::Key::Enter));
                        true
                    }
                }
            } else {
                note(
                    ui,
                    "One file with your accounts, saved passwords and sign-ins, settings, signatures, rules, categories, send-only addresses, calendar accounts and layout, to import on your other PC. The mail itself is not in it: the other PC fetches it.",
                );
                note(ui, "The whole file is locked with this password (at least 8 characters). MailCraft cannot recover it if it is forgotten.");
                enter |= field(ui, "Password", &mut f.pass, "tr_p1");
                enter |= field(ui, "Again", &mut f.pass2, "tr_p2");
                if !f.pass2.is_empty() && f.pass != f.pass2 {
                    ui.label(RichText::new("The two do not match.").color(t.red));
                }
                f.pass.chars().count() >= 8 && f.pass == f.pass2
            };
            if let Some(e) = &f.error {
                ui.add(egui::Label::new(RichText::new(e).color(t.red)).wrap());
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let label = match (f.import, f.bundle.is_some()) {
                    (true, false) => "Unlock",
                    (true, true) => "Import",
                    _ => "Export...",
                };
                let b = egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(t.accent).min_size(vec2(90.0, 28.0));
                if ui.add_enabled(ready, b).clicked() || (ready && enter) {
                    go = true;
                }
                if ui.add(egui::Button::new("Cancel").min_size(vec2(80.0, 28.0))).clicked() {
                    close = true;
                }
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
        });
        if close {
            return;
        }
        if go {
            if f.done.is_some() {
                if f.restart {
                    self.allow_close = true;
                    restart(ctx);
                }
                return;
            }
            let dir = crate::accounts::config_dir();
            if f.import {
                match (&f.locked, &f.bundle) {
                    (Some(l), None) => match unlock(l, &f.pass) {
                        Ok(b) => {
                            f.bundle = Some(b);
                            f.error = None;
                        }
                        Err(e) => f.error = Some(e),
                    },
                    (_, Some(b)) => match apply(b, &dir) {
                        Ok(backup) => {
                            f.error = None;
                            f.restart = true;
                            f.done = Some(format!(
                                "Imported. MailCraft starts again to use the new settings.\n\nWhat was here before is in {}",
                                backup.display()
                            ));
                        }
                        Err(e) => f.error = Some(e),
                    },
                    _ => {}
                }
                f.pass.clear();
            } else {
                let name = format!("MailCraft settings ({} {}).mcsettings", this_pc(), chrono::Local::now().format("%Y-%m-%d"));
                if let Some(path) = test_file().or_else(|| {
                    rfd::FileDialog::new()
                        .set_title("Export MailCraft settings")
                        .set_file_name(&name)
                        .add_filter("MailCraft settings", &["mcsettings"])
                        .save_file()
                }) {
                    let b = gather(&dir);
                    let r = lock(&b, &f.pass, ITERATIONS)
                        .and_then(|text| std::fs::write(&path, text).map_err(|e| format!("could not write {}: {e}", path.display())));
                    match r {
                        Ok(()) => {
                            f.pass.clear();
                            f.pass2.clear();
                            let n = b.files.get("accounts.json").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                            let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
                            f.done = Some(format!(
                                "Exported {} and {}, locked with your password, to\n{}\n\nOn the other PC: File > Import settings.",
                                plural(n, "account", "accounts"),
                                plural(b.secrets.len(), "saved password or sign-in", "saved passwords and sign-ins"),
                                path.display()
                            ));
                        }
                        Err(e) => f.error = Some(e),
                    }
                }
            }
        }
        self.transfer = Some(f);
    }
}

/// For automated tests: the settings file to use instead of asking (MAILCRAFT_TEST_SETTINGS_FILE).
fn test_file() -> Option<PathBuf> {
    std::env::var_os("MAILCRAFT_TEST_SETTINGS_FILE").map(PathBuf::from)
}

/// Starts MailCraft again (a new copy) and closes this one.
fn restart(ctx: &egui::Context) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seals_and_opens() {
        let s = seal(b"{\"a@b\":\"pw\"}", "correct horse", 1000).unwrap();
        assert_eq!(open(&s, "correct horse").unwrap(), b"{\"a@b\":\"pw\"}");
        assert!(open(&s, "wrong horse").is_err());
        let mut t = s.clone();
        t.data.replace_range(0..1, if t.data.starts_with('A') { "B" } else { "A" });
        assert!(open(&t, "correct horse").is_err(), "tampering is caught");
    }

    #[test]
    fn round_trip_keeps_local_window() {
        let base = std::env::temp_dir().join(format!("mc-transfer-{}", std::process::id()));
        let (a, b) = (base.join("a"), base.join("b"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("accounts.json"), r#"[{"email":"me@example.org"}]"#).unwrap();
        std::fs::write(a.join("rules.json"), "[1,2]").unwrap();
        std::fs::write(a.join("view.json"), r#"{"theme":"Dark","window":{"x":1},"maximized":true}"#).unwrap();
        std::fs::write(b.join("view.json"), r#"{"theme":"Light","window":{"x":9}}"#).unwrap();
        std::fs::write(b.join("rules.json"), "[]").unwrap();
        let mut bundle = gather(&a);
        assert!(bundle.files.get("view.json").is_some_and(|v| v.get("window").is_none()), "screen place stays behind");
        bundle.secrets.insert("me@example.org".into(), "s3cret-value".into());
        let path = base.join("x.mcsettings");
        let text = lock(&bundle, "a long password", 1000).unwrap();
        // Nothing readable without the password.
        let raw = String::from_utf8_lossy(&text);
        assert!(!raw.contains("example.org") && !raw.contains("s3cret") && !raw.contains("Dark"), "{raw}");
        std::fs::write(&path, &text).unwrap();
        let locked = read(&path).unwrap();
        assert!(unlock(&locked, "not it").is_err());
        let back = unlock(&locked, "a long password").unwrap();
        assert!(describe(&back).iter().any(|l| l.contains("me@example.org")));
        assert!(describe(&back).iter().any(|l| l.contains("1 saved password")));
        assert_eq!(back.secrets.get("me@example.org").map(String::as_str), Some("s3cret-value"));
        let mut no_secrets = back.clone();
        no_secrets.secrets.clear();
        let backup = apply(&no_secrets, &b).unwrap();
        let view: serde_json::Value = serde_json::from_slice(&std::fs::read(b.join("view.json")).unwrap()).unwrap();
        assert_eq!(view.get("theme").and_then(|v| v.as_str()), Some("Dark"));
        assert_eq!(view.get("window").and_then(|w| w.get("x")).and_then(|x| x.as_i64()), Some(9), "this PC's window kept");
        assert_eq!(std::fs::read_to_string(backup.join("rules.json")).unwrap(), "[]", "old settings backed up");
        assert!(read(&b.join("view.json")).is_err(), "not a settings file");
        let _ = std::fs::remove_dir_all(&base);
    }
}
