//! OAuth 2.0 sign-in for Gmail and Microsoft (Outlook.com, Hotmail, Microsoft 365).
//!
//! The installed-app flow of RFC 8252: the sign-in page opens in the system browser, the
//! provider redirects back to a one-shot listener on the loopback interface, and PKCE (RFC 7636)
//! ties the code to this sign-in. The refresh token that comes back is what MailCraft keeps;
//! access tokens last about an hour and are fetched again with it.
//!
//! Each provider needs an app registration of its own (a client ID, and for Google a client
//! "secret", which for desktop apps Google does not treat as secret). They come from the
//! person's own registrations, kept in the app's settings, not from this code.

use crate::Error;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Google,
    Microsoft,
}

impl Provider {
    /// Which sign-in an IMAP server needs, if it is one of the OAuth-only providers.
    pub fn for_host(imap_host: &str) -> Option<Provider> {
        let h = imap_host.trim_end_matches('.').to_ascii_lowercase();
        if h == "imap.gmail.com" || h == "imap.googlemail.com" {
            return Some(Provider::Google);
        }
        if ["outlook.office365.com", "outlook.office.com", "imap-mail.outlook.com", "imap.outlook.com"].contains(&h.as_str()) {
            return Some(Provider::Microsoft);
        }
        None
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::Google => "Google",
            Provider::Microsoft => "Microsoft",
        }
    }

    /// Google's desktop clients come with a client secret that the token requests must carry.
    pub fn needs_secret(self) -> bool {
        self == Provider::Google
    }

    fn scope(self, purpose: Purpose) -> &'static str {
        match (self, purpose) {
            (Provider::Google, Purpose::Mail) => "https://mail.google.com/ openid email",
            (Provider::Microsoft, Purpose::Mail) => {
                "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access openid email"
            }
            // Google's CalDAV takes the Calendar API scope.
            (Provider::Google, Purpose::Calendar) => "https://www.googleapis.com/auth/calendar openid email",
            // Microsoft has no CalDAV: calendars are read and written through Microsoft Graph.
            (Provider::Microsoft, Purpose::Calendar) => "https://graph.microsoft.com/Calendars.ReadWrite offline_access openid email",
        }
    }

    fn auth_url(self, c: &Client) -> String {
        c.auth_url.clone().unwrap_or_else(|| match self {
            Provider::Google => "https://accounts.google.com/o/oauth2/v2/auth".into(),
            Provider::Microsoft => format!("https://login.microsoftonline.com/{}/oauth2/v2.0/authorize", c.tenant()),
        })
    }

    fn token_url(self, c: &Client) -> String {
        c.token_url.clone().unwrap_or_else(|| match self {
            Provider::Google => "https://oauth2.googleapis.com/token".into(),
            Provider::Microsoft => format!("https://login.microsoftonline.com/{}/oauth2/v2.0/token", c.tenant()),
        })
    }

    /// Google wants the loopback address written as an IP; Microsoft matches "http://localhost".
    fn redirect_host(self) -> &'static str {
        match self {
            Provider::Google => "127.0.0.1",
            Provider::Microsoft => "localhost",
        }
    }
}

/// What a sign-in is for. Each needs its own permission from the provider, and Microsoft hands
/// out tokens for one service at a time (its mail servers, or Graph), so a calendar is a sign-in
/// of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Mail,
    Calendar,
}

/// One app registration with a provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Client {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// Microsoft only: "common" (work and personal accounts), "consumers", or a tenant ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Overrides for testing against a stand-in server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
}

impl Client {
    fn tenant(&self) -> &str {
        self.tenant.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or("common")
    }

    /// The client ID as the provider expects it. Pasting from a portal easily brings along a
    /// colon, quotes or the label beside it ("Application (client) ID : b9de..."), and the
    /// provider then says the app does not exist; a Microsoft ID is the GUID in whatever came.
    pub fn id(&self) -> String {
        if let Some(g) = find_guid(&self.client_id) {
            return g.to_string();
        }
        self.client_id.trim().trim_matches(|c: char| c == ':' || c == '"' || c == '\'' || c.is_whitespace()).to_string()
    }

    pub fn secret(&self) -> Option<String> {
        self.client_secret
            .as_deref()
            .map(|s| s.trim().trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace()).to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn is_complete(&self, p: Provider) -> bool {
        let id = self.id();
        let id_ok = match p {
            Provider::Microsoft => find_guid(&id).is_some(),
            Provider::Google => !id.is_empty() && !id.contains(char::is_whitespace),
        };
        id_ok && (!p.needs_secret() || self.secret().is_some())
    }
}

/// The first GUID (8-4-4-4-12 hex digits) in `s`.
fn find_guid(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    let shape =
        |w: &[u8]| w.len() == 36 && w.iter().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { *c == b'-' } else { c.is_ascii_hexdigit() });
    (0..b.len().saturating_sub(35)).find(|&i| b.get(i..i + 36).is_some_and(shape)).and_then(|i| s.get(i..i + 36))
}

/// What a token endpoint gives back.
#[derive(Debug, Clone)]
pub struct Tokens {
    pub access_token: String,
    /// Present on sign-in; on refresh only when the provider hands out a new one (Microsoft does).
    pub refresh_token: Option<String>,
    pub expires_in: Duration,
    /// The address the person actually signed in as, from the ID token, when there is one.
    pub email: Option<String>,
}

/// How long to wait for the person to finish in the browser.
pub const SIGN_IN_WAIT: Duration = Duration::from_secs(300);

/// Sign in for mail: open the provider's page with `open_browser`, wait for the redirect, swap the
/// code for tokens.
pub fn sign_in(p: Provider, c: &Client, login_hint: &str, open_browser: impl FnOnce(&str) -> Result<(), String>) -> Result<Tokens, Error> {
    sign_in_for(p, c, login_hint, Purpose::Mail, open_browser)
}

/// Sign in for `purpose` (see `sign_in`).
pub fn sign_in_for(
    p: Provider,
    c: &Client,
    login_hint: &str,
    purpose: Purpose,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Tokens, Error> {
    if !c.is_complete(p) {
        return Err(Error::OAuth(format!("MailCraft has no {} app registration yet", p.name())));
    }
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    // "localhost" may resolve to ::1 first; listen there too on the same port when possible.
    let v6 = TcpListener::bind(("::1", port)).ok();
    let redirect = format!("http://{}:{port}/", p.redirect_host());
    let verifier = random_token(32)?;
    let challenge = URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()));
    let state = random_token(16)?;
    let client_id = c.id();
    let mut q = vec![
        ("response_type", "code"),
        ("client_id", client_id.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("scope", p.scope(purpose)),
        ("state", state.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("login_hint", login_hint),
    ];
    if p == Provider::Google {
        // Ask for a refresh token every time, even when this app was allowed before.
        q.push(("access_type", "offline"));
        q.push(("prompt", "consent"));
    }
    let url = format!("{}?{}", p.auth_url(c), form(&q));
    open_browser(&url).map_err(|e| Error::OAuth(format!("could not open the browser: {e}")))?;
    let code = wait_for_code(&listener, v6.as_ref(), &state, SIGN_IN_WAIT)?;
    let mut form_fields = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("client_id", client_id.as_str()),
        ("code_verifier", verifier.as_str()),
        ("scope", p.scope(purpose)),
    ];
    let secret = c.secret().filter(|_| p.needs_secret());
    if let Some(s) = secret.as_deref() {
        form_fields.push(("client_secret", s));
    }
    let t = token_request(&p.token_url(c), &form_fields)?;
    if t.refresh_token.is_none() {
        return Err(Error::OAuth(format!("{} signed you in but gave no refresh token, so MailCraft could not stay signed in", p.name())));
    }
    Ok(t)
}

/// A fresh access token (for mail) from a refresh token.
pub fn refresh(p: Provider, c: &Client, refresh_token: &str) -> Result<Tokens, Error> {
    refresh_for(p, c, refresh_token, Purpose::Mail)
}

/// A fresh access token for `purpose` from a refresh token.
pub fn refresh_for(p: Provider, c: &Client, refresh_token: &str, purpose: Purpose) -> Result<Tokens, Error> {
    let client_id = c.id();
    let mut f =
        vec![("grant_type", "refresh_token"), ("refresh_token", refresh_token), ("client_id", client_id.as_str()), ("scope", p.scope(purpose))];
    let secret = c.secret().filter(|_| p.needs_secret());
    if let Some(s) = secret.as_deref() {
        f.push(("client_secret", s));
    }
    token_request(&p.token_url(c), &f)
}

/// The XOAUTH2 initial response (before base64): used for IMAP AUTHENTICATE and SMTP AUTH.
pub fn xoauth2(user: &str, access_token: &str) -> String {
    format!("user={user}\x01auth=Bearer {access_token}\x01\x01")
}

/// True when a refresh failed because the grant is gone (revoked, expired, password changed):
/// only signing in again helps.
pub fn is_revoked(e: &Error) -> bool {
    matches!(e, Error::OAuth(m) if m.contains("invalid_grant"))
}

fn random_token(bytes: usize) -> Result<String, Error> {
    use ring::rand::SecureRandom;
    let mut b = vec![0u8; bytes];
    ring::rand::SystemRandom::new().fill(&mut b).map_err(|_| Error::OAuth("no random numbers available".into()))?;
    Ok(URL_SAFE_NO_PAD.encode(b))
}

fn pct(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

fn unpct(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        let hex = || b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match c {
            b'+' => out.push(b' '),
            b'%' => match hex() {
                Some(v) => {
                    out.push(v);
                    i += 2;
                }
                None => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", pct(k), pct(v))).collect::<Vec<_>>().join("&")
}

fn query(path: &str) -> Vec<(String, String)> {
    let q = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    q.split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| kv.split_once('=').map(|(k, v)| (unpct(k), unpct(v))).unwrap_or_else(|| (unpct(kv), String::new())))
        .collect()
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>MailCraft</title><body style=\"font:16px system-ui,sans-serif;margin:60px auto;max-width:520px;color:#222\"><h2>Signed in</h2><p>You can close this tab and go back to MailCraft.</p>";

fn reply(s: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    let _ = s.flush();
}

fn page(title: &str, msg: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        "<!doctype html><meta charset=utf-8><title>MailCraft</title><body style=\"font:16px system-ui,sans-serif;margin:60px auto;max-width:520px;color:#222\"><h2>{}</h2><p>{}</p>",
        esc(title),
        esc(msg)
    )
}

/// Serve the loopback redirect: the first request carrying our state ends the wait.
fn wait_for_code(v4: &TcpListener, v6: Option<&TcpListener>, state: &str, wait: Duration) -> Result<String, Error> {
    v4.set_nonblocking(true)?;
    if let Some(l) = v6 {
        l.set_nonblocking(true)?;
    }
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        let accepted = match v4.accept() {
            Ok(a) => Some(a),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => v6.and_then(|l| l.accept().ok()),
            Err(e) => return Err(e.into()),
        };
        let Some((mut s, _)) = accepted else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        s.set_nonblocking(false)?;
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut line = String::new();
        if BufReader::new(Read::take(&mut s, 8192)).read_line(&mut line).is_err() {
            continue;
        }
        let path = line.split_whitespace().nth(1).unwrap_or("");
        let q = query(path);
        let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        if get("state").as_deref() != Some(state) {
            // The browser asking for /favicon.ico, or a stray request: not ours.
            reply(&mut s, "404 Not Found", "");
            continue;
        }
        if let Some(err) = get("error") {
            let why = get("error_description").unwrap_or_else(|| err.clone());
            reply(&mut s, "200 OK", &page("Not signed in", &why));
            return Err(Error::OAuth(if err == "access_denied" { "sign-in was cancelled".into() } else { why }));
        }
        let Some(code) = get("code").filter(|c| !c.is_empty()) else {
            reply(&mut s, "400 Bad Request", &page("Not signed in", "The reply had no sign-in code."));
            return Err(Error::OAuth("the provider's reply had no sign-in code".into()));
        };
        reply(&mut s, "200 OK", DONE_PAGE);
        return Ok(code);
    }
    Err(Error::OAuth("timed out waiting for the browser sign-in".into()))
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    id_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn token_request(url: &str, fields: &[(&str, &str)]) -> Result<Tokens, Error> {
    let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).http_status_as_error(false).build().new_agent();
    let mut resp = agent
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(form(fields))
        .map_err(|e| Error::OAuth(format!("token request: {e}")))?;
    let status = resp.status().as_u16();
    let body = resp.body_mut().with_config().limit(1 << 20).read_to_string().map_err(|e| Error::OAuth(format!("token reply: {e}")))?;
    let r: TokenReply =
        serde_json::from_str(&body).map_err(|_| Error::OAuth(format!("token endpoint answered {status} with something that is not JSON")))?;
    if let Some(e) = r.error {
        return Err(Error::OAuth(match r.error_description {
            Some(d) => format!("{e}: {}", d.lines().next().unwrap_or("")),
            None => e,
        }));
    }
    let access_token =
        r.access_token.filter(|t| !t.is_empty()).ok_or_else(|| Error::OAuth(format!("token endpoint answered {status} without an access token")))?;
    Ok(Tokens {
        access_token,
        refresh_token: r.refresh_token,
        expires_in: Duration::from_secs(r.expires_in.unwrap_or(3600)),
        email: r.id_token.as_deref().and_then(id_email),
    })
}

/// The signed-in address from an ID token's claims. The token came straight from the token
/// endpoint over TLS, so it is read, not verified; it only labels the account.
fn id_email(jwt: &str) -> Option<String> {
    let claims = jwt.split('.').nth(1)?;
    let raw = URL_SAFE_NO_PAD.decode(claims.trim_end_matches('=')).or_else(|_| STANDARD.decode(claims)).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    ["email", "preferred_username", "upn"].iter().find_map(|k| v.get(*k).and_then(|e| e.as_str()).filter(|e| e.contains('@')).map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_the_oauth_providers() {
        assert_eq!(Provider::for_host("imap.gmail.com"), Some(Provider::Google));
        assert_eq!(Provider::for_host("Outlook.Office365.com"), Some(Provider::Microsoft));
        assert_eq!(Provider::for_host("imap.fastmail.com"), None);
    }

    #[test]
    fn encodes_and_decodes() {
        assert_eq!(pct("a b/c+d@e"), "a%20b%2Fc%2Bd%40e");
        assert_eq!(unpct("a%20b%2Fc+d%40e%"), "a b/c d@e%");
        let q = query("/?code=4%2F0Ab&state=xyz&scope=email+openid");
        assert_eq!(q[0], ("code".into(), "4/0Ab".into()));
        assert_eq!(q[2].1, "email openid");
        assert_eq!(xoauth2("me@example.com", "tok"), "user=me@example.com\x01auth=Bearer tok\x01\x01");
    }

    #[test]
    fn cleans_pasted_client_ids() {
        let ms = |id: &str| Client { client_id: id.into(), ..Default::default() };
        let g = "b9dedab8-0e32-4471-864a-f3c190f6f270";
        for pasted in [
            g,
            ": b9dedab8-0e32-4471-864a-f3c190f6f270",
            "Application (client) ID : b9dedab8-0e32-4471-864a-f3c190f6f270 ",
            "\"b9dedab8-0e32-4471-864a-f3c190f6f270\"",
        ] {
            assert_eq!(ms(pasted).id(), g, "{pasted}");
            assert!(ms(pasted).is_complete(Provider::Microsoft));
        }
        assert!(!ms("b9dedab8-0e32-4471").is_complete(Provider::Microsoft));
        let google =
            Client { client_id: " 123-abc.apps.googleusercontent.com\n".into(), client_secret: Some(" GOCSPX-x ".into()), ..Default::default() };
        assert_eq!(google.id(), "123-abc.apps.googleusercontent.com");
        assert_eq!(google.secret().as_deref(), Some("GOCSPX-x"));
        assert!(google.is_complete(Provider::Google));
        assert!(!Client { client_secret: None, ..google }.is_complete(Provider::Google));
    }

    #[test]
    fn reads_the_email_from_an_id_token() {
        let claims = URL_SAFE_NO_PAD.encode(br#"{"iss":"x","email":"me@example.com"}"#);
        assert_eq!(id_email(&format!("h.{claims}.s")).as_deref(), Some("me@example.com"));
        assert_eq!(id_email("junk"), None);
    }

    #[test]
    fn the_loopback_listener_takes_only_our_redirect() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            for path in ["/favicon.ico", "/?state=other&code=bad", "/?state=s1&code=good%2F1"] {
                let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
                write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
                let mut r = String::new();
                let _ = std::io::Read::read_to_string(&mut s, &mut r);
            }
        });
        assert_eq!(wait_for_code(&l, None, "s1", Duration::from_secs(10)).unwrap(), "good/1");
        t.join().unwrap();
    }
}
