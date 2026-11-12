//! Account passwords and OAuth refresh tokens. On Windows they live in Credential Manager
//! ("MailCraft" / the address); elsewhere, for now, only in memory for the session.
//!
//! Credential Manager holds at most 1280 characters per entry, and Microsoft's refresh tokens
//! can be longer, so a long value is split over "address#1", "address#2"... with the main
//! entry saying how many parts there are.

use std::collections::HashMap;
use std::sync::Mutex;

static SESSION: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

pub fn get(email: &str) -> Option<String> {
    if let Some(p) = SESSION.lock().ok().and_then(|m| m.as_ref().and_then(|m| m.get(email).cloned())) {
        return Some(p);
    }
    #[cfg(windows)]
    {
        if let Some(p) = read(email) {
            return Some(p);
        }
    }
    // Automated tests against a local test server only.
    std::env::var("MAILCRAFT_TEST_PASSWORD").ok()
}

/// Remember a password. Returns where it was kept.
pub fn set(email: &str, password: &str) -> &'static str {
    if let Ok(mut m) = SESSION.lock() {
        m.get_or_insert_with(HashMap::new).insert(email.to_string(), password.to_string());
    }
    #[cfg(windows)]
    {
        if write(email, password).is_some() {
            return "Windows Credential Manager";
        }
    }
    "memory (until MailCraft closes)"
}

#[cfg(windows)]
const PART: usize = 1000;
#[cfg(windows)]
const PARTS: &str = "mailcraft-parts:";

#[cfg(windows)]
fn read(email: &str) -> Option<String> {
    // No saved sign-in is ordinary; one Windows would not hand over is worth knowing about,
    // because the timed check for mail then skips the account without a word.
    let head = match keyring::Entry::new("MailCraft", email).and_then(|e| e.get_password()) {
        Ok(h) => h,
        Err(keyring::Error::NoEntry) => return None,
        Err(e) => {
            log::warn!("Credential Manager would not give the saved sign-in for {email}: {e}");
            return None;
        }
    };
    let Some(n) = head.strip_prefix(PARTS).and_then(|n| n.parse::<usize>().ok()) else { return Some(head) };
    let mut whole = String::new();
    for i in 1..=n.min(64) {
        whole.push_str(&keyring::Entry::new("MailCraft", &format!("{email}#{i}")).ok()?.get_password().ok()?);
    }
    Some(whole)
}

#[cfg(windows)]
fn write(email: &str, value: &str) -> Option<()> {
    let head = keyring::Entry::new("MailCraft", email).ok()?;
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= PART {
        return head.set_password(value).ok();
    }
    let parts: Vec<String> = chars.chunks(PART).map(|c| c.iter().collect()).collect();
    for (i, p) in parts.iter().enumerate() {
        keyring::Entry::new("MailCraft", &format!("{email}#{}", i + 1)).ok()?.set_password(p).ok()?;
    }
    head.set_password(&format!("{PARTS}{}", parts.len())).ok()
}

/// Forget an account's saved password or sign-in (all its parts).
pub fn forget(email: &str) {
    if let Ok(mut m) = SESSION.lock()
        && let Some(m) = m.as_mut()
    {
        m.remove(email);
    }
    #[cfg(windows)]
    {
        for i in 1..=64 {
            match keyring::Entry::new("MailCraft", &format!("{email}#{i}")) {
                Ok(e) if e.delete_credential().is_ok() => {}
                _ => break,
            }
        }
        if let Ok(e) = keyring::Entry::new("MailCraft", email) {
            let _ = e.delete_credential();
        }
    }
}
