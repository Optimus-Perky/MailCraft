//! The accounts MailCraft knows: where their servers are and what to log in as.
//! Passwords are not in here; see `secrets`.

use mailcraft_autoconfig::Security;
use mailcraft_core::oauth::{Client, Provider};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerCfg {
    pub host: String,
    pub port: u16,
    pub security: Security,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    pub username: String,
    pub imap: ServerCfg,
    pub smtp: Option<ServerCfg>,
    /// Test servers only: allow an unencrypted connection.
    #[serde(default)]
    pub allow_insecure: bool,
    /// Signs in through the provider's page (Gmail, Outlook.com, Microsoft 365) instead of a
    /// password; the secret kept for it is then the OAuth refresh token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<Provider>,
    /// What the folder list calls it (the address when empty), and its colour there.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<[u8; 3]>,
    /// Sent mail: where replies go (when not to this address), and the organisation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reply_to: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub organisation: String,
}

impl Account {
    /// What to call it: its name, else its address.
    pub fn title(&self) -> &str {
        if self.label.trim().is_empty() { &self.email } else { self.label.trim() }
    }
}

impl Account {
    pub fn smtp_endpoint(&self) -> Option<mailcraft_core::Endpoint> {
        self.smtp.as_ref().map(|s| mailcraft_core::Endpoint {
            host: s.host.clone(),
            port: s.port,
            security: s.security,
            allow_insecure: self.allow_insecure,
        })
    }

    pub fn imap_endpoint(&self) -> mailcraft_core::Endpoint {
        mailcraft_core::Endpoint {
            host: self.imap.host.clone(),
            port: self.imap.port,
            security: self.imap.security,
            allow_insecure: self.allow_insecure,
        }
    }
}

/// An address to send from without its mail being here (no folders): just its own outgoing
/// (SMTP) server (senders.json). Its password is kept like an account's, as "smtp:<address>".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sender {
    pub email: String,
    #[serde(default)]
    pub name: String,
    pub smtp: ServerCfg,
    pub username: String,
    /// Offered when writing from this account; empty: from any account.
    #[serde(default)]
    pub account: String,
    /// Test servers only.
    #[serde(default)]
    pub allow_insecure: bool,
    /// Signs in through the provider (Outlook.com, Gmail) rather than with a password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<Provider>,
}

/// Which provider's sign-in an address or outgoing server needs, if one of the OAuth ones.
pub fn provider_for(email: &str, smtp_host: &str) -> Option<Provider> {
    let h = smtp_host.trim().to_ascii_lowercase();
    if ["smtp-mail.outlook.com", "smtp.office365.com", "smtp.outlook.com"].contains(&h.as_str()) {
        return Some(Provider::Microsoft);
    }
    if h == "smtp.gmail.com" || h == "smtp.googlemail.com" {
        return Some(Provider::Google);
    }
    let domain = email.rsplit('@').next().unwrap_or("").to_ascii_lowercase();
    let ms = ["outlook.", "hotmail.", "live.", "msn.com", "windowslive.com"];
    if ms.iter().any(|d| domain.starts_with(d) || domain == *d) {
        Some(Provider::Microsoft)
    } else if domain == "gmail.com" || domain == "googlemail.com" {
        Some(Provider::Google)
    } else {
        None
    }
}

impl Sender {
    /// The address as an account, for its provider sign-in (the token is kept under the address).
    pub fn as_account(&self) -> Account {
        Account {
            email: self.email.clone(),
            username: self.username.clone(),
            imap: ServerCfg { host: String::new(), port: 993, security: self.smtp.security },
            smtp: Some(self.smtp.clone()),
            allow_insecure: self.allow_insecure,
            oauth: self.oauth,
            label: String::new(),
            colour: None,
            reply_to: String::new(),
            organisation: String::new(),
        }
    }

    /// Where its secret is kept: the provider's sign-in under the address (as an account's),
    /// a password under "smtp:<address>".
    pub fn secret_name(&self) -> String {
        if self.oauth.is_some() { self.email.clone() } else { self.secret_key() }
    }
    pub fn secret_key(&self) -> String {
        format!("smtp:{}", self.email.trim().to_ascii_lowercase())
    }

    pub fn endpoint(&self) -> mailcraft_core::Endpoint {
        mailcraft_core::Endpoint {
            host: self.smtp.host.clone(),
            port: self.smtp.port,
            security: self.smtp.security,
            allow_insecure: self.allow_insecure,
        }
    }

    /// "Name <address>", or the address.
    pub fn label(&self) -> String {
        if self.name.trim().is_empty() { self.email.clone() } else { format!("{} <{}>", self.name.trim(), self.email) }
    }
}

pub fn load_senders() -> Vec<Sender> {
    read_settings("senders.json").unwrap_or_default()
}

pub fn save_senders(senders: &[Sender]) -> std::io::Result<()> {
    write_settings("senders.json", &senders)
}

/// Settings (accounts.json) go in the roaming profile; the mail cache in the local one.
pub fn config_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("MailCraft")
}

pub fn cache_path() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    };
    let dir = base.unwrap_or_else(|| PathBuf::from(".")).join("MailCraft");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("mail.db")
}

/// Read a settings file, accepting the byte-order mark Notepad or PowerShell may have added.
fn read_json<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    let b = std::fs::read(config_dir().join(name)).ok()?;
    serde_json::from_slice(b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&b)).ok()
}

/// Other settings files beside accounts.json (view.json: how the window is laid out).
pub fn read_settings<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    read_json(name)
}

pub fn write_settings<T: Serialize>(name: &str, value: &T) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(name), serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)
}

/// A colour category (categories.json). `key` is the IMAP keyword stored on messages; it
/// stays the same when the category is renamed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Category {
    pub name: String,
    pub colour: [u8; 3],
    pub key: String,
}

/// A name made into an IMAP keyword (an atom: letters, digits, '_' and '-').
pub fn keyword(name: &str) -> String {
    let k: String = name.trim().chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    let k = k.trim_matches('_').to_string();
    if k.is_empty() { "Category".into() } else { k }
}

pub fn default_categories() -> Vec<Category> {
    [
        ("Red category", [0xE7, 0x48, 0x56]),
        ("Orange category", [0xF7, 0x63, 0x0C]),
        ("Yellow category", [0xF2, 0xB7, 0x05]),
        ("Green category", [0x16, 0xA3, 0x4A]),
        ("Blue category", [0x00, 0x78, 0xD4]),
        ("Purple category", [0x88, 0x6C, 0xE4]),
    ]
    .into_iter()
    .map(|(n, c)| Category { name: n.into(), colour: c, key: keyword(n) })
    .collect()
}

pub fn load_categories() -> Vec<Category> {
    read_json("categories.json").unwrap_or_else(default_categories)
}

pub fn save_categories(c: &[Category]) -> std::io::Result<()> {
    write_settings("categories.json", &c)
}

/// A folder of an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderRef {
    pub account: String,
    pub folder: String,
}

/// Favourite folders (in order), folder colours, and folders that raise no new-mail
/// notifications (folders.json).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderPrefs {
    pub favourites: Vec<FolderRef>,
    pub colours: Vec<(FolderRef, [u8; 3])>,
    pub quiet: Vec<FolderRef>,
}

impl FolderPrefs {
    pub fn colour(&self, account: &str, folder: &str) -> Option<[u8; 3]> {
        self.colours.iter().find(|(r, _)| r.account == account && r.folder == folder).map(|(_, c)| *c)
    }

    /// Left out of new-mail notifications (its right-click menu).
    pub fn is_quiet(&self, account: &str, folder: &str) -> bool {
        self.quiet.iter().any(|r| r.account == account && r.folder == folder)
    }

    pub fn toggle_quiet(&mut self, account: &str, folder: &str) {
        if self.is_quiet(account, folder) {
            self.quiet.retain(|r| !(r.account == account && r.folder == folder));
        } else {
            self.quiet.push(FolderRef { account: account.to_string(), folder: folder.to_string() });
        }
    }

    pub fn is_favourite(&self, account: &str, folder: &str) -> bool {
        self.favourites.iter().any(|r| r.account == account && r.folder == folder)
    }

    /// A folder (and its subfolders) renamed: keep its settings.
    pub fn renamed(&mut self, account: &str, from: &str, to: &str, delim: &str) {
        let fix = |r: &mut FolderRef| {
            if r.account != account {
                return;
            }
            if r.folder == from {
                r.folder = to.to_string();
            } else if let Some(rest) = r.folder.strip_prefix(&format!("{from}{delim}")) {
                r.folder = format!("{to}{delim}{rest}");
            }
        };
        self.favourites.iter_mut().for_each(fix);
        self.colours.iter_mut().for_each(|(r, _)| fix(r));
        self.quiet.iter_mut().for_each(fix);
    }

    /// A folder (and its subfolders) deleted, or an account removed (`folder` None).
    pub fn forget(&mut self, account: &str, folder: Option<&str>, delim: &str) {
        let gone = |r: &FolderRef| r.account == account && folder.is_none_or(|f| r.folder == f || r.folder.starts_with(&format!("{f}{delim}")));
        self.favourites.retain(|r| !gone(r));
        self.colours.retain(|(r, _)| !gone(r));
        self.quiet.retain(|r| !gone(r));
    }
}

pub fn load_folder_prefs() -> FolderPrefs {
    read_json("folders.json").unwrap_or_default()
}

pub fn save_folder_prefs(p: &FolderPrefs) -> std::io::Result<()> {
    write_settings("folders.json", p)
}

/// Mail rules for every account (rules.json).
pub fn load_rules() -> Vec<mailcraft_core::rules::Rule> {
    read_json("rules.json").unwrap_or_default()
}

pub fn save_rules(rules: &[mailcraft_core::rules::Rule]) -> std::io::Result<()> {
    write_settings("rules.json", &rules)
}

pub fn load() -> Vec<Account> {
    read_json("accounts.json").unwrap_or_default()
}

pub fn save(accounts: &[Account]) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_vec_pretty(accounts).map_err(std::io::Error::other)?;
    std::fs::write(dir.join("accounts.json"), json)
}

/// The person's own app registrations with Google and Microsoft (oauth.json beside accounts.json).
pub fn oauth_clients() -> HashMap<Provider, Client> {
    read_json("oauth.json").unwrap_or_default()
}

/// The registration to sign in with: the person's own when they have one (so sign-ins made
/// with it keep working), else the one built into this copy of MailCraft.
pub fn oauth_client(p: Provider) -> Option<Client> {
    pick_client(p, oauth_clients().remove(&p), builtin_client(p))
}

/// True when the person has a usable registration of their own for this provider.
pub fn has_own_oauth_client(p: Provider) -> bool {
    oauth_clients().remove(&p).is_some_and(|c| c.is_complete(p))
}

fn pick_client(p: Provider, own: Option<Client>, builtin: Option<Client>) -> Option<Client> {
    own.filter(|c| c.is_complete(p)).or_else(|| builtin.filter(|c| c.is_complete(p)))
}

/// The registration compiled into this build (see build.rs), if any. Only Google so far.
pub fn builtin_client(p: Provider) -> Option<Client> {
    match p {
        Provider::Google => builtin_google(option_env!("MAILCRAFT_BUILTIN_GOOGLE_ID"), option_env!("MAILCRAFT_BUILTIN_GOOGLE_SECRET")),
        Provider::Microsoft => None,
    }
}

fn builtin_google(id: Option<&str>, secret: Option<&str>) -> Option<Client> {
    let c = Client { client_id: id?.to_string(), client_secret: Some(secret?.to_string()), ..Default::default() };
    c.is_complete(Provider::Google).then_some(c)
}

pub fn save_oauth_client(p: Provider, c: Client) -> std::io::Result<()> {
    let mut all = oauth_clients();
    all.insert(p, c);
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("oauth.json"), serde_json::to_vec_pretty(&all).map_err(std::io::Error::other)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn google(id: &str, secret: &str) -> Client {
        Client { client_id: id.into(), client_secret: Some(secret.into()), ..Default::default() }
    }

    #[test]
    fn own_registration_wins_then_builtin() {
        let own = google("own-1.apps.googleusercontent.com", "GOCSPX-own");
        let built = google("built-2.apps.googleusercontent.com", "GOCSPX-built");
        let pick = |o: Option<Client>, b: Option<Client>| pick_client(Provider::Google, o, b).map(|c| c.client_id);
        assert_eq!(pick(Some(own.clone()), Some(built.clone())).as_deref(), Some("own-1.apps.googleusercontent.com"));
        assert_eq!(pick(None, Some(built.clone())).as_deref(), Some("built-2.apps.googleusercontent.com"));
        // A half-filled registration of their own does not hide the built-in one.
        let half = Client { client_secret: None, ..own };
        assert_eq!(pick(Some(half), Some(built)).as_deref(), Some("built-2.apps.googleusercontent.com"));
        assert_eq!(pick(None, None), None);
    }

    #[test]
    fn builtin_needs_both_values() {
        assert!(builtin_google(Some("x.apps.googleusercontent.com"), Some("GOCSPX-s")).is_some());
        assert!(builtin_google(Some("x.apps.googleusercontent.com"), None).is_none());
        assert!(builtin_google(None, Some("GOCSPX-s")).is_none());
        assert!(builtin_google(Some(""), Some("GOCSPX-s")).is_none());
        // Microsoft is never built in.
        assert!(builtin_client(Provider::Microsoft).is_none());
    }

    /// Run with MAILCRAFT_OAUTH_FILE set at build time and MAILCRAFT_EXPECT_BUILTIN_ID at run
    /// time to prove build.rs compiled the registration in; without them, that none is.
    #[test]
    fn builtin_matches_the_build() {
        let got = builtin_client(Provider::Google).map(|c| c.client_id);
        match std::env::var("MAILCRAFT_EXPECT_BUILTIN_ID") {
            Ok(want) => assert_eq!(got.as_deref(), Some(want.as_str())),
            Err(_) if std::env::var_os("MAILCRAFT_OAUTH_FILE").is_none() => assert_eq!(got, None),
            Err(_) => {}
        }
    }
}
