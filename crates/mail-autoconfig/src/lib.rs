//! Account autoconfiguration: from an email address to its IMAP and SMTP servers.
//!
//! The same lookups Thunderbird makes, in the same order: the provider's own autoconfig file
//! (`autoconfig.<domain>`, then `<domain>/.well-known/`), then Mozilla's public provider database
//! (ISPDB). The first answer wins. All three speak the same XML ("config-v1.1").
//!
//! [`discover`] goes further when none of those knows the domain (a self-hosted server, say):
//! DNS SRV records, the ISPDB entry for the MX host's domain, then testing the usual names and
//! ports for a real IMAP or SMTP greeting. See [`probe`].

mod probe;

use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not an email address: {0}")]
    BadAddress(String),
    #[error("no settings found for {0} (tried {1} places)")]
    NotFound(String, usize),
}

/// How a connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Security {
    /// TLS from the first byte (IMAP 993, SMTP 465).
    Tls,
    /// Plain connection upgraded with STARTTLS (IMAP 143, SMTP 587).
    StartTls,
    /// No encryption. Listed by some providers; MailCraft will refuse it unless told otherwise.
    None,
}

#[derive(Debug, Clone, Serialize)]
pub struct Server {
    /// "imap", "pop3" or "smtp".
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// What to log in as, with the address already substituted in.
    pub username: String,
    /// Authentication methods the provider lists: "password-cleartext", "OAuth2", ...
    pub auth: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    /// Where the answer came from.
    pub source: String,
    pub provider: Option<String>,
    pub incoming: Vec<Server>,
    pub outgoing: Vec<Server>,
}

impl Settings {
    /// The IMAP server to use: the first one listed that is encrypted.
    pub fn imap(&self) -> Option<&Server> {
        self.incoming.iter().find(|s| s.protocol == "imap" && s.security != Security::None)
    }
    /// The SMTP server to use: the first one listed that is encrypted.
    pub fn smtp(&self) -> Option<&Server> {
        self.outgoing.iter().find(|s| s.protocol == "smtp" && s.security != Security::None)
    }
}

/// The places to ask, in order.
pub fn candidates(email: &str, domain: &str) -> Vec<(String, String)> {
    vec![
        (format!("https://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress={email}"), format!("autoconfig.{domain}")),
        (format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml?emailaddress={email}"), format!("{domain}/.well-known")),
        (format!("https://autoconfig.thunderbird.net/v1.1/{domain}"), "Mozilla ISPDB".to_string()),
    ]
}

/// Split an address into its local part and domain.
pub fn split(email: &str) -> Result<(&str, &str), Error> {
    let email = email.trim();
    match email.rsplit_once('@') {
        Some((local, domain)) if !local.is_empty() && domain.contains('.') => Ok((local, domain)),
        _ => Err(Error::BadAddress(email.to_string())),
    }
}

/// Look the address up. Network errors on one candidate just move on to the next.
pub fn lookup(email: &str) -> Result<Settings, Error> {
    let (_, domain) = split(email)?;
    let domain = domain.to_ascii_lowercase();
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(8))).build().into();
    let tries = candidates(email, &domain);
    for (url, source) in &tries {
        let Ok(mut resp) = agent.get(url).call() else { continue };
        let Ok(body) = resp.body_mut().read_to_string() else { continue };
        if let Some(mut s) = parse(&body, email) {
            if s.incoming.is_empty() && s.outgoing.is_empty() {
                continue;
            }
            s.source = source.clone();
            return Ok(s);
        }
    }
    Err(Error::NotFound(email.to_string(), tries.len()))
}

/// Everything [`lookup`] does, then the fallbacks for domains no directory knows.
pub async fn discover(email: &str) -> Result<Settings, Error> {
    let owned = email.to_string();
    if let Ok(Ok(s)) = tokio::task::spawn_blocking(move || lookup(&owned)).await {
        return Ok(s);
    }
    let (_, domain) = split(email)?;
    let domain = domain.to_ascii_lowercase();
    let Some(resolver) = probe::resolver() else { return Err(Error::NotFound(email.to_string(), 3)) };

    // 1. SRV records (RFC 6186), each one checked before it is trusted.
    let (imap_srv, smtp_srv) = probe::srv(&resolver, &domain).await;
    let mut imap = None;
    for (h, p, sec) in imap_srv {
        if probe::speaks(&h, p, sec, true).await {
            imap = Some(Server {
                protocol: "imap".into(),
                host: h,
                port: p,
                security: sec,
                username: email.to_string(),
                auth: vec!["password-cleartext".into()],
            });
            break;
        }
    }
    let mut smtp = None;
    for (h, p, sec) in smtp_srv {
        if probe::speaks(&h, p, sec, false).await {
            smtp = Some(Server {
                protocol: "smtp".into(),
                host: h,
                port: p,
                security: sec,
                username: email.to_string(),
                auth: vec!["password-cleartext".into()],
            });
            break;
        }
    }
    if let (Some(i), Some(o)) = (&imap, &smtp) {
        return Ok(Settings { source: "DNS SRV records".into(), provider: None, incoming: vec![i.clone()], outgoing: vec![o.clone()] });
    }

    // 2. The MX host's domain in the ISPDB: a domain hosted by Google or Microsoft is recognised
    //    by where its mail goes, not by its own name.
    let mx = probe::mx(&resolver, &domain).await;
    for host in &mx {
        let parts: Vec<&str> = host.split('.').collect();
        if parts.len() < 2 {
            continue;
        }
        let mx_domain = parts[parts.len() - 2..].join(".");
        if mx_domain == domain {
            continue;
        }
        let url = format!("https://autoconfig.thunderbird.net/v1.1/{mx_domain}");
        let addr = email.to_string();
        let got = tokio::task::spawn_blocking(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(8))).build().into();
            agent.get(&url).call().ok().and_then(|mut r| r.body_mut().read_to_string().ok()).and_then(|b| parse(&b, &addr))
        })
        .await
        .ok()
        .flatten();
        if let Some(mut s) = got
            && !s.incoming.is_empty()
        {
            s.source = format!("Mozilla ISPDB via MX {host}");
            return Ok(s);
        }
    }

    // 3. Test the usual names and ports.
    if imap.is_none() {
        imap = probe::guess(&domain, &mx, true, email).await;
    }
    if smtp.is_none() {
        smtp = probe::guess(&domain, &mx, false, email).await;
    }
    match (imap, smtp) {
        (None, None) => Err(Error::NotFound(email.to_string(), 6)),
        (i, o) => Ok(Settings {
            source: "tested the usual servers".into(),
            provider: None,
            incoming: i.into_iter().collect(),
            outgoing: o.into_iter().collect(),
        }),
    }
}

/// Parse a config-v1.1 document, filling in the address placeholders.
pub fn parse(xml: &str, email: &str) -> Option<Settings> {
    use quick_xml::events::Event;
    let (local, domain) = split(email).ok()?;
    let fill = |v: &str| v.replace("%EMAILADDRESS%", email).replace("%EMAILLOCALPART%", local).replace("%EMAILDOMAIN%", domain);

    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out = Settings { source: String::new(), provider: None, incoming: vec![], outgoing: vec![] };
    let mut current: Option<(bool, Server)> = None; // (is_outgoing, server being read)
    let mut tag = String::new();
    let mut seen_root = false;
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if name == "clientConfig" {
                    seen_root = true;
                }
                if name == "incomingServer" || name == "outgoingServer" {
                    let protocol = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.local_name().as_ref() == b"type")
                        .map(|a| String::from_utf8_lossy(&a.value).to_ascii_lowercase())
                        .unwrap_or_default();
                    current = Some((
                        name == "outgoingServer",
                        Server { protocol, host: String::new(), port: 0, security: Security::None, username: String::new(), auth: vec![] },
                    ));
                }
                tag = name;
            }
            Event::Text(t) => {
                let text = t.decode().ok()?.into_owned();
                match (&mut current, tag.as_str()) {
                    (Some((_, s)), "hostname") => s.host = fill(&text),
                    (Some((_, s)), "port") => s.port = text.parse().unwrap_or(0),
                    (Some((_, s)), "socketType") => {
                        s.security = match text.to_ascii_uppercase().as_str() {
                            "SSL" | "TLS" => Security::Tls,
                            "STARTTLS" => Security::StartTls,
                            _ => Security::None,
                        }
                    }
                    (Some((_, s)), "username") => s.username = fill(&text),
                    (Some((_, s)), "authentication") => s.auth.push(text),
                    (None, "displayName") if out.provider.is_none() => out.provider = Some(text),
                    _ => {}
                }
            }
            Event::End(e) => {
                let name = e.local_name();
                if (name.as_ref() == b"incomingServer" || name.as_ref() == b"outgoingServer")
                    && let Some((outgoing, s)) = current.take()
                    && !s.host.is_empty()
                    && s.port != 0
                {
                    if outgoing { out.outgoing.push(s) } else { out.incoming.push(s) }
                }
                tag.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    seen_root.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0"?>
<clientConfig version="1.1">
  <emailProvider id="example.com">
    <domain>example.com</domain>
    <displayName>Example Mail</displayName>
    <incomingServer type="imap">
      <hostname>imap.example.com</hostname><port>993</port><socketType>SSL</socketType>
      <authentication>password-cleartext</authentication><authentication>OAuth2</authentication>
      <username>%EMAILADDRESS%</username>
    </incomingServer>
    <incomingServer type="pop3">
      <hostname>pop.example.com</hostname><port>110</port><socketType>plain</socketType>
      <username>%EMAILLOCALPART%</username><authentication>password-cleartext</authentication>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.%EMAILDOMAIN%</hostname><port>587</port><socketType>STARTTLS</socketType>
      <username>%EMAILADDRESS%</username><authentication>password-cleartext</authentication>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#;

    #[test]
    fn reads_servers_and_fills_placeholders() {
        let s = parse(SAMPLE, "someone@example.com").expect("parses");
        assert_eq!(s.provider.as_deref(), Some("Example Mail"));
        let imap = s.imap().expect("imap");
        assert_eq!((imap.host.as_str(), imap.port, imap.security), ("imap.example.com", 993, Security::Tls));
        assert_eq!(imap.username, "someone@example.com");
        assert_eq!(imap.auth, vec!["password-cleartext", "OAuth2"]);
        assert_eq!(s.incoming[1].username, "someone");
        let smtp = s.smtp().expect("smtp");
        assert_eq!((smtp.host.as_str(), smtp.port, smtp.security), ("smtp.example.com", 587, Security::StartTls));
    }

    #[test]
    fn not_xml_is_none() {
        assert!(parse("<html>404</html>", "a@b.com").is_none());
        assert!(split("nobody").is_err());
    }
}
