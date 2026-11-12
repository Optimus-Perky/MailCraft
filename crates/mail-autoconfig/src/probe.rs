//! When no autoconfig file exists: DNS SRV records (RFC 6186), Mozilla's ISPDB looked up by the
//! MX host's domain, and finally testing the usual host names on the usual ports.
//!
//! A probe only counts when the server answers with the right greeting over the right security.
//! A name that merely resolves is not enough: on many domains `imap.<domain>` is a wildcard
//! pointing at a web proxy (Cloudflare, for instance), which never speaks IMAP.

use crate::{Security, Server};
use hickory_resolver::TokioResolver;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;

const STEP: Duration = Duration::from_secs(6);

pub(crate) fn resolver() -> Option<TokioResolver> {
    TokioResolver::builder_tokio().ok().map(|b| b.build())
}

/// SRV records for IMAP over TLS, submission over TLS and submission with STARTTLS.
pub(crate) async fn srv(resolver: &TokioResolver, domain: &str) -> (Vec<(String, u16, Security)>, Vec<(String, u16, Security)>) {
    let mut imap = vec![];
    let mut smtp = vec![];
    for (name, list_is_imap, sec) in [
        (format!("_imaps._tcp.{domain}."), true, Security::Tls),
        (format!("_imap._tcp.{domain}."), true, Security::StartTls),
        (format!("_submissions._tcp.{domain}."), false, Security::Tls),
        (format!("_submission._tcp.{domain}."), false, Security::StartTls),
    ] {
        let Ok(found) = resolver.srv_lookup(name.as_str()).await else { continue };
        for r in found.iter() {
            let host = r.target().to_utf8().trim_end_matches('.').to_string();
            // RFC 6186: a target of "." means the service is deliberately not offered.
            if host.is_empty() || r.port() == 0 {
                continue;
            }
            if list_is_imap { imap.push((host, r.port(), sec)) } else { smtp.push((host, r.port(), sec)) }
        }
    }
    (imap, smtp)
}

/// The domain's MX hosts, best preference first.
pub(crate) async fn mx(resolver: &TokioResolver, domain: &str) -> Vec<String> {
    let Ok(found) = resolver.mx_lookup(format!("{domain}.").as_str()).await else { return vec![] };
    let mut v: Vec<(u16, String)> = found.iter().map(|m| (m.preference(), m.exchange().to_utf8().trim_end_matches('.').to_string())).collect();
    v.sort();
    v.into_iter().map(|(_, h)| h).filter(|h| !h.is_empty()).collect()
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap_or_else(|_| rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13]))
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

async fn line<R: AsyncRead + Unpin>(r: &mut BufReader<R>) -> Option<String> {
    let mut s = String::new();
    match timeout(STEP, r.read_line(&mut s)).await {
        Ok(Ok(n)) if n > 0 => Some(s),
        _ => None,
    }
}

/// Read an IMAP greeting, or an SMTP greeting (all of a multi-line 220).
async fn greeting<R: AsyncRead + Unpin>(r: &mut BufReader<R>, imap: bool) -> bool {
    let Some(first) = line(r).await else { return false };
    if imap {
        return first.starts_with("* OK") || first.starts_with("* PREAUTH");
    }
    let mut l = first;
    loop {
        if !l.starts_with("220") {
            return false;
        }
        if l.as_bytes().get(3) != Some(&b'-') {
            return true;
        }
        match line(r).await {
            Some(n) => l = n,
            None => return false,
        }
    }
}

/// After a plain greeting, does the server offer STARTTLS?
async fn offers_starttls<S: AsyncRead + AsyncWrite + Unpin>(r: &mut BufReader<S>, imap: bool) -> bool {
    let ask: &[u8] = if imap { b"a1 CAPABILITY\r\n" } else { b"EHLO mailcraft.invalid\r\n" };
    if timeout(STEP, r.get_mut().write_all(ask)).await.is_err() {
        return false;
    }
    let mut seen = false;
    for _ in 0..40 {
        let Some(l) = line(r).await else { return false };
        if l.to_ascii_uppercase().contains("STARTTLS") {
            seen = true;
        }
        let done = if imap { l.starts_with("a1 ") } else { l.len() > 3 && l.as_bytes().get(3) == Some(&b' ') };
        if done {
            return seen;
        }
    }
    false
}

/// Does `host:port` speak IMAP (or SMTP) with the given security?
pub(crate) async fn speaks(host: &str, port: u16, security: Security, imap: bool) -> bool {
    let Ok(Ok(tcp)) = timeout(STEP, TcpStream::connect((host, port))).await else { return false };
    match security {
        Security::Tls => {
            let Ok(name) = rustls::pki_types::ServerName::try_from(host.to_string()) else { return false };
            let connector = tokio_rustls::TlsConnector::from(tls_config());
            let Ok(Ok(tls)) = timeout(STEP, connector.connect(name, tcp)).await else { return false };
            greeting(&mut BufReader::new(tls), imap).await
        }
        Security::StartTls => {
            let mut r = BufReader::new(tcp);
            greeting(&mut r, imap).await && offers_starttls(&mut r, imap).await
        }
        Security::None => false,
    }
}

/// Test the usual names and ports, all at once; return the first that answers, in preference order.
pub(crate) async fn guess(domain: &str, mx_hosts: &[String], imap: bool, user: &str) -> Option<Server> {
    let mut hosts: Vec<String> =
        if imap { vec![format!("imap.{domain}"), format!("mail.{domain}")] } else { vec![format!("smtp.{domain}"), format!("mail.{domain}")] };
    hosts.extend(mx_hosts.iter().cloned());
    hosts.push(domain.to_string());
    hosts.dedup();
    let ports: &[(u16, Security)] =
        if imap { &[(993, Security::Tls), (143, Security::StartTls)] } else { &[(465, Security::Tls), (587, Security::StartTls)] };
    let mut tries = vec![];
    for h in &hosts {
        for &(p, s) in ports {
            tries.push((h.clone(), p, s));
        }
    }
    let mut set = tokio::task::JoinSet::new();
    for (i, (h, p, s)) in tries.iter().cloned().enumerate() {
        set.spawn(async move { (i, speaks(&h, p, s, imap).await) });
    }
    let mut ok = vec![false; tries.len()];
    while let Some(Ok((i, good))) = set.join_next().await {
        if let Some(slot) = ok.get_mut(i) {
            *slot = good;
        }
    }
    tries.into_iter().zip(ok).find(|(_, good)| *good).map(|((host, port, security), _)| Server {
        protocol: if imap { "imap" } else { "smtp" }.to_string(),
        host,
        port,
        security,
        username: user.to_string(),
        auth: vec!["password-cleartext".to_string()],
    })
}
