//! Windows' own notifications, for new mail and reminders while MailCraft is minimised.
//!
//! The corner card (`draw_card`) is drawn in MailCraft's UI pass, and eframe runs no UI pass for
//! a minimised window (only `logic`), so a card raised then never appeared and the mail went
//! unannounced. While the window is minimised the card goes to Windows instead: silent (no
//! sounds: a quiet notice is enough), with MailCraft's name and icon, and a click brings
//! MailCraft forward at the message or the reminder's day, as clicking the card does.

use std::sync::mpsc::Sender;

/// A notification that was clicked: what its card would have opened.
pub struct Clicked {
    pub open: Option<(String, u32)>,
    pub day: Option<chrono::NaiveDate>,
}

/// The identity Windows shows the notifications under (registered in `register`).
#[cfg(windows)]
const APP_ID: &str = "uk.perkin.MailCraft";

/// Show the card as a Windows notification. False when that is not possible here (not
/// Windows, or Windows refused), so the caller keeps the card.
#[cfg(windows)]
pub fn show(card: &crate::Card, tx: Sender<Clicked>, ctx: egui::Context) -> bool {
    register();
    let mut toast = tauri_winrt_notification::Toast::new(APP_ID).title(&card.title).sound(None);
    // Three lines: the title, the card's first line, then its second or the heading.
    let mut lines = card.lines.iter();
    if let Some(first) = lines.next() {
        toast = toast.text1(first);
    }
    toast = toast.text2(lines.next().map(String::as_str).unwrap_or(&card.heading));
    let (open, day) = (card.open.clone(), card.day);
    toast = toast.on_activated(move |_| {
        let _ = tx.send(Clicked { open: open.clone(), day });
        ctx.request_repaint();
        Ok(())
    });
    toast.show().is_ok()
}

#[cfg(not(windows))]
pub fn show(_card: &crate::Card, _tx: Sender<Clicked>, _ctx: egui::Context) -> bool {
    false
}

/// Tell Windows who `APP_ID` is (name and icon), so notifications say "MailCraft" without
/// MailCraft having to be a packaged app. Per user, once a run.
#[cfg(windows)]
fn register() {
    static DONE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    DONE.get_or_init(|| {
        let icon = crate::accounts::cache_path().with_file_name("notification-icon.png");
        let _ = std::fs::write(&icon, include_bytes!("../../../assets/app-icon/mailcraft-256.png"));
        if let Ok(key) = windows_registry::CURRENT_USER.create(format!("Software\\Classes\\AppUserModelId\\{APP_ID}")) {
            let _ = key.set_string("DisplayName", "MailCraft");
            let _ = key.set_string("IconUri", icon.to_string_lossy().as_ref());
        }
    });
}
