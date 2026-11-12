//! Opening an IMAP session over TLS, STARTTLS or (only when explicitly allowed) plain TCP.

use crate::Error;
use base64::Engine;
use mailcraft_autoconfig::Security;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Any byte stream async-imap can run over: TLS or (test servers) plain.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug> Stream for T {}

pub type Session = async_imap::Session<Box<dyn Stream>>;

/// Where and how to connect.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// Plain IMAP is refused unless this is set. Only for local test servers.
    pub allow_insecure: bool,
}

/// How to log in: a password, or an OAuth access token (SASL XOAUTH2).
#[derive(Clone, Copy)]
pub enum Login<'a> {
    Password(&'a str),
    OAuth2(&'a str),
}

impl std::fmt::Debug for Login<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Login::Password(_) => "Password(..)",
            Login::OAuth2(_) => "OAuth2(..)",
        })
    }
}

/// Answers the XOAUTH2 challenge once; after a failure the server sends an error as a second
/// challenge, and the empty reply ends the exchange with a NO.
struct XOAuth2(String);

impl async_imap::Authenticator for XOAuth2 {
    type Response = String;
    fn process(&mut self, _challenge: &[u8]) -> String {
        std::mem::take(&mut self.0)
    }
}

const STEP: Duration = Duration::from_secs(20);

pub(crate) fn tls_config() -> Arc<rustls::ClientConfig> {
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

async fn tls(host: &str, tcp: TcpStream) -> Result<Box<dyn Stream>, Error> {
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|e| Error::Tls(e.to_string()))?;
    let s = timeout(STEP, tokio_rustls::TlsConnector::from(tls_config()).connect(name, tcp))
        .await
        .map_err(|_| Error::Tls("handshake timed out".into()))?
        .map_err(|e| Error::Tls(e.to_string()))?;
    Ok(Box::new(s))
}

/// Connect and log in with a password. The password is used for LOGIN and not kept.
pub async fn connect(ep: &Endpoint, user: &str, password: &str) -> Result<Session, Error> {
    connect_with(ep, user, Login::Password(password)).await
}

/// Signing in: a server that took the connection and then says nothing must not hold up every
/// request queued behind this one.
const SIGN_IN: Duration = Duration::from_secs(60);

/// Connect and log in with a password or an OAuth access token. Each connection goes in the
/// log with how long connecting and signing in took (never what it signed in with).
pub async fn connect_with(ep: &Endpoint, user: &str, login: Login<'_>) -> Result<Session, Error> {
    let began = std::time::Instant::now();
    let mut greeted_at = 0;
    let r = connect_steps(ep, user, login, began, &mut greeted_at).await;
    let ms = began.elapsed().as_millis();
    match &r {
        Ok(_) => log::info!("{user} at {}:{}: connected and greeted in {greeted_at} ms, signed in at {ms} ms", ep.host, ep.port),
        Err(e) => log::warn!("{user} at {}:{}: {e} (after {ms} ms)", ep.host, ep.port),
    }
    r
}

async fn connect_steps(ep: &Endpoint, user: &str, login: Login<'_>, began: std::time::Instant, greeted_at: &mut u128) -> Result<Session, Error> {
    let tcp = timeout(STEP, TcpStream::connect((ep.host.as_str(), ep.port)))
        .await
        .map_err(|_| Error::Connect(ep.host.clone(), "timed out".into()))?
        .map_err(|e| Error::Connect(ep.host.clone(), e.to_string()))?;
    let (stream, greeted): (Box<dyn Stream>, bool) = match ep.security {
        Security::Tls => (tls(&ep.host, tcp).await?, false),
        Security::StartTls => {
            // Upgrade by hand, then give the TLS stream to async-imap. There is no second greeting.
            let mut r = BufReader::new(tcp);
            let mut l = String::new();
            timeout(STEP, r.read_line(&mut l)).await.map_err(|_| Error::Imap("no greeting".into()))??;
            if !l.starts_with("* OK") {
                return Err(Error::Imap(format!("unexpected greeting: {}", l.trim())));
            }
            r.get_mut().write_all(b"a0 STARTTLS\r\n").await?;
            loop {
                l.clear();
                timeout(STEP, r.read_line(&mut l)).await.map_err(|_| Error::Imap("STARTTLS timed out".into()))??;
                if l.starts_with("a0 ") {
                    break;
                }
            }
            if !l.starts_with("a0 OK") {
                return Err(Error::Tls(format!("server refused STARTTLS: {}", l.trim())));
            }
            (tls(&ep.host, r.into_inner()).await?, true)
        }
        Security::None => {
            if !ep.allow_insecure {
                return Err(Error::Insecure(ep.host.clone()));
            }
            (Box::new(tcp), false)
        }
    };
    let mut client = async_imap::Client::new(stream);
    if !greeted {
        let _greeting = timeout(STEP, client.read_response()).await.map_err(|_| Error::Imap("no greeting".into()))?;
    }
    *greeted_at = began.elapsed().as_millis();
    timeout(SIGN_IN, sign_in(client, user, login))
        .await
        .map_err(|_| Error::Login(format!("the server did not answer the sign-in within {} seconds", SIGN_IN.as_secs())))?
}

async fn sign_in(client: async_imap::Client<Box<dyn Stream>>, user: &str, login: Login<'_>) -> Result<Session, Error> {
    match login {
        Login::Password(password) => client.login(user, password).await.map_err(|(e, _)| Error::Login(e.to_string())),
        Login::OAuth2(token) => {
            // Token on the command line itself (SASL-IR, which Gmail and Outlook offer); a
            // server that rejects that form gets the classic challenge-and-response instead.
            let ir = base64::engine::general_purpose::STANDARD.encode(crate::oauth::xoauth2(user, token));
            match client.authenticate(format!("XOAUTH2 {ir}"), XOAuth2(String::new())).await {
                Ok(s) => Ok(s),
                Err((async_imap::error::Error::Bad(_), client)) => {
                    client.authenticate("XOAUTH2", XOAuth2(crate::oauth::xoauth2(user, token))).await.map_err(|(e, _)| Error::Login(e.to_string()))
                }
                Err((e, _)) => Err(Error::Login(e.to_string())),
            }
        }
    }
}
