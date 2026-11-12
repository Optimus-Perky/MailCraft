//! Writing mail out: build the MIME message, hand it to the SMTP server, and keep a copy in Sent.

use crate::Error;
use crate::connect::{Endpoint, Login, Session};
use crate::store::Store;
use futures::TryStreamExt;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart, header::ContentType};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use mailcraft_autoconfig::Security;

/// A message ready to go.
#[derive(Debug, Clone, Default)]
pub struct Outgoing {
    pub from: String,
    pub from_name: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub html: String,
    pub text: String,
    /// The Message-ID this replies to, and the thread so far (oldest first).
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// (file name, MIME type, contents)
    pub attachments: Vec<(String, String, Vec<u8>)>,
    /// Message-ID to use; drafts keep one so a newer save can replace the older.
    pub message_id: Option<String>,
    /// An automatic reply: marked Auto-Submitted (RFC 3834) so other auto-repliers stay quiet.
    pub auto_reply: bool,
    /// Where replies should go, when not to the sender.
    pub reply_to: Option<String>,
    /// The Organization header.
    pub organisation: Option<String>,
    /// A meeting request, reply or cancellation (iMIP, RFC 6047): (METHOD, iCalendar text). It
    /// goes beside the text as a text/calendar alternative, where mail programs look for it.
    pub calendar: Option<(String, String)>,
}

/// The Organization header (RFC 2076).
#[derive(Debug, Clone)]
struct Organisation(String);

impl lettre::message::header::Header for Organisation {
    fn name() -> lettre::message::header::HeaderName {
        lettre::message::header::HeaderName::new_from_ascii_str("Organization")
    }

    fn parse(s: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Organisation(s.to_string()))
    }

    fn display(&self) -> lettre::message::header::HeaderValue {
        lettre::message::header::HeaderValue::new(Self::name(), self.0.clone())
    }
}

/// RFC 3834's Auto-Submitted header, as automatic replies carry it.
#[derive(Debug, Clone)]
struct AutoSubmitted;

impl lettre::message::header::Header for AutoSubmitted {
    fn name() -> lettre::message::header::HeaderName {
        lettre::message::header::HeaderName::new_from_ascii_str("Auto-Submitted")
    }

    fn parse(_: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(AutoSubmitted)
    }

    fn display(&self) -> lettre::message::header::HeaderValue {
        lettre::message::header::HeaderValue::new(Self::name(), "auto-replied".to_string())
    }
}

/// Split "a@x, B <b@y>; c@z" into addresses.
pub fn split_addresses(s: &str) -> Vec<String> {
    s.split([',', ';']).map(str::trim).filter(|a| !a.is_empty()).map(str::to_string).collect()
}

fn mailbox(s: &str) -> Result<Mailbox, Error> {
    s.parse::<Mailbox>().map_err(|e| Error::Send(format!("bad address \"{s}\": {e}")))
}

fn bracketed(id: &str) -> String {
    let id = id.trim();
    if id.starts_with('<') { id.to_string() } else { format!("<{id}>") }
}

/// Build the MIME message: HTML with a plain-text alternative, plus any attachments.
pub fn build(o: &Outgoing) -> Result<Message, Error> {
    if o.to.is_empty() && o.cc.is_empty() && o.bcc.is_empty() {
        return Err(Error::Send("there is nobody to send it to".into()));
    }
    build_any(o)
}

/// A draft may have nobody to send to yet; it is never handed to SMTP, so the envelope
/// (which needs a recipient) just names the sender.
pub fn build_draft(o: &Outgoing) -> Result<Message, Error> {
    build_any(o)
}

fn build_any(o: &Outgoing) -> Result<Message, Error> {
    let from = match &o.from_name {
        Some(n) if !n.is_empty() => format!("{n} <{}>", o.from),
        _ => o.from.clone(),
    };
    let from_box = mailbox(&from)?;
    let mut b = Message::builder().from(from_box.clone()).subject(o.subject.clone());
    if o.to.is_empty() && o.cc.is_empty() && o.bcc.is_empty() {
        let env =
            lettre::address::Envelope::new(Some(from_box.email.clone()), vec![from_box.email.clone()]).map_err(|e| Error::Send(e.to_string()))?;
        b = b.envelope(env);
    }
    if let Some(id) = &o.message_id {
        b = b.message_id(Some(bracketed(id)));
    }
    for a in &o.to {
        b = b.to(mailbox(a)?);
    }
    for a in &o.cc {
        b = b.cc(mailbox(a)?);
    }
    for a in &o.bcc {
        b = b.bcc(mailbox(a)?);
    }
    if let Some(id) = &o.in_reply_to {
        b = b.in_reply_to(bracketed(id));
    }
    if !o.references.is_empty() {
        b = b.references(o.references.iter().map(|r| bracketed(r)).collect::<Vec<_>>().join(" "));
    }
    if o.auto_reply {
        b = b.header(AutoSubmitted);
    }
    if let Some(r) = o.reply_to.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        b = b.reply_to(mailbox(r)?);
    }
    if let Some(org) = o.organisation.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
        b = b.header(Organisation(org.to_string()));
    }
    // No HTML: a plain-text message (the person chose plain text).
    let plain = o.html.trim().is_empty();
    let text_part = || SinglePart::plain(o.text.clone());
    if let Some((method, ics)) = &o.calendar {
        let ct = ContentType::parse(&format!("text/calendar; charset=utf-8; method={}", method.trim().to_ascii_uppercase()))
            .map_err(|e| Error::Send(e.to_string()))?;
        let mut alt = MultiPart::alternative().singlepart(text_part());
        if !plain {
            alt = alt.singlepart(SinglePart::html(o.html.clone()));
        }
        alt = alt.singlepart(SinglePart::builder().header(ct).body(ics.clone()));
        let msg = if o.attachments.is_empty() {
            b.multipart(alt)
        } else {
            let mut mixed = MultiPart::mixed().multipart(alt);
            for (name, mime, bytes) in &o.attachments {
                let ct = ContentType::parse(mime).unwrap_or(ContentType::parse("application/octet-stream").map_err(|e| Error::Send(e.to_string()))?);
                mixed = mixed.singlepart(Attachment::new(name.clone()).body(bytes.clone(), ct));
            }
            b.multipart(mixed)
        };
        return msg.map_err(|e| Error::Send(e.to_string()));
    }
    let msg = if o.attachments.is_empty() {
        if plain { b.singlepart(text_part()) } else { b.multipart(MultiPart::alternative_plain_html(o.text.clone(), o.html.clone())) }
    } else {
        let mut mixed = if plain {
            MultiPart::mixed().singlepart(text_part())
        } else {
            MultiPart::mixed().multipart(MultiPart::alternative_plain_html(o.text.clone(), o.html.clone()))
        };
        for (name, mime, bytes) in &o.attachments {
            let ct = ContentType::parse(mime).unwrap_or(ContentType::parse("application/octet-stream").map_err(|e| Error::Send(e.to_string()))?);
            mixed = mixed.singlepart(Attachment::new(name.clone()).body(bytes.clone(), ct));
        }
        b.multipart(mixed)
    };
    msg.map_err(|e| Error::Send(e.to_string()))
}

/// Hand the message to the SMTP server.
pub async fn smtp_send(ep: &Endpoint, user: &str, login: Login<'_>, msg: &Message) -> Result<(), Error> {
    transport(ep, user, login)?.send(msg.clone()).await.map(|_| ()).map_err(|e| Error::Send(e.to_string()))
}

/// Connect to an SMTP server and sign in, sending nothing (the Test button for a send-only
/// address).
pub async fn smtp_test(ep: &Endpoint, user: &str, login: Login<'_>) -> Result<(), Error> {
    match transport(ep, user, login)?.test_connection().await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Error::Send("the server did not answer".into())),
        Err(e) => Err(Error::Send(e.to_string())),
    }
}

fn transport(ep: &Endpoint, user: &str, login: Login<'_>) -> Result<AsyncSmtpTransport<Tokio1Executor>, Error> {
    let (creds, mechanisms) = match login {
        Login::Password(p) => (Credentials::new(user.to_string(), p.to_string()), vec![Mechanism::Plain, Mechanism::Login]),
        Login::OAuth2(t) => (Credentials::new(user.to_string(), t.to_string()), vec![Mechanism::Xoauth2]),
    };
    let transport = match ep.security {
        Security::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&ep.host)
            .map_err(|e| Error::Send(e.to_string()))?
            .port(ep.port)
            .credentials(creds)
            .authentication(mechanisms)
            .build(),
        Security::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&ep.host)
            .map_err(|e| Error::Send(e.to_string()))?
            .port(ep.port)
            .credentials(creds)
            .authentication(mechanisms)
            .build(),
        Security::None => {
            if !ep.allow_insecure {
                return Err(Error::Insecure(ep.host.clone()));
            }
            // Test servers only: no TLS, so no password over it; a test token is fine.
            let b = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&ep.host).port(ep.port);
            if matches!(login, Login::OAuth2(_)) { b.credentials(creds).authentication(mechanisms).build() } else { b.build() }
        }
    };
    Ok(transport)
}

/// The account's Sent folder: the one marked \Sent, else one named like it, else "Sent" (created).
pub async fn sent_folder(session: &mut Session, store: &Store, account: &str) -> Result<String, Error> {
    let folders = store.folders(account)?;
    if let Some(f) = folders.iter().find(|f| f.special.as_deref() == Some("\\Sent")) {
        return Ok(f.name.clone());
    }
    for guess in ["Sent", "Sent Items", "Sent Messages", "INBOX.Sent", "INBOX/Sent"] {
        if folders.iter().any(|f| f.name.eq_ignore_ascii_case(guess)) {
            return Ok(guess.to_string());
        }
    }
    let _ = session.create("Sent").await;
    Ok("Sent".into())
}

/// The account's Drafts folder: the one marked \Drafts, else one named like it, else "Drafts" (created).
pub async fn drafts_folder(session: &mut Session, store: &Store, account: &str) -> Result<String, Error> {
    let folders = store.folders(account)?;
    if let Some(f) = folders.iter().find(|f| f.special.as_deref() == Some("\\Drafts")) {
        return Ok(f.name.clone());
    }
    for guess in ["Drafts", "Draft", "INBOX.Drafts", "INBOX/Drafts"] {
        if let Some(f) = folders.iter().find(|f| f.name.eq_ignore_ascii_case(guess)) {
            return Ok(f.name.clone());
        }
    }
    let _ = session.create("Drafts").await;
    Ok("Drafts".into())
}

/// UIDs in the selected folder carrying this Message-ID.
async fn uids_with_id(session: &mut Session, id: &str) -> Result<Vec<u32>, Error> {
    let q = format!("HEADER Message-ID \"{}\"", bracketed(id).replace('"', ""));
    Ok(session.uid_search(q).await?.into_iter().collect())
}

/// Delete these UIDs from the selected folder (UID EXPUNGE where offered, else EXPUNGE).
async fn remove_uids(session: &mut Session, uids: &[u32]) -> Result<(), Error> {
    if uids.is_empty() {
        return Ok(());
    }
    let set = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let _: Vec<_> = session.uid_store(&set, "+FLAGS (\\Deleted)").await?.try_collect().await?;
    let by_uid: Result<Vec<_>, _> = match session.uid_expunge(&set).await {
        Ok(s) => s.try_collect().await,
        Err(e) => Err(e),
    };
    if by_uid.is_err() {
        let _: Vec<_> = session.expunge().await?.try_collect().await?;
    }
    Ok(())
}

/// Save (or replace) a draft: the new copy goes in first, then older copies with the same
/// Message-ID are removed, so there is never a moment with no copy at all.
pub async fn save_draft(session: &mut Session, store: &Store, account: &str, raw: &[u8], id: &str) -> Result<String, Error> {
    let folder = drafts_folder(session, store, account).await?;
    session.select(&folder).await?;
    let old = uids_with_id(session, id).await?;
    session.append(&folder, Some("(\\Draft \\Seen)"), None, raw).await.map_err(|e| Error::Imap(e.to_string()))?;
    session.select(&folder).await?;
    remove_uids(session, &old).await?;
    Ok(folder)
}

/// Remove a draft (after it was sent, or when it is discarded).
pub async fn delete_draft(session: &mut Session, store: &Store, account: &str, id: &str) -> Result<(), Error> {
    let folder = drafts_folder(session, store, account).await?;
    session.select(&folder).await?;
    let uids = uids_with_id(session, id).await?;
    remove_uids(session, &uids).await
}

/// Keep a copy of what was sent, already marked read.
pub async fn append(session: &mut Session, folder: &str, raw: &[u8]) -> Result<(), Error> {
    session.append(folder, Some("(\\Seen)"), None, raw).await.map_err(|e| Error::Imap(e.to_string()))
}

/// Add or remove flags on one message, e.g. ("+FLAGS", "\\Seen") or ("+FLAGS", "\\Answered").
pub async fn store_flags(session: &mut Session, folder: &str, uid: u32, op: &str, flags: &str) -> Result<(), Error> {
    session.select(folder).await?;
    let _done: Vec<_> = session.uid_store(uid.to_string(), format!("{op} ({flags})")).await?.try_collect().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn carries_a_meeting_reply() {
        let o = Outgoing {
            from: "me@example.com".into(),
            to: vec!["boss@example.org".into()],
            subject: "Accepted: Planning".into(),
            text: "Mark has accepted.".into(),
            calendar: Some(("REPLY".into(), "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nEND:VCALENDAR\r\n".into())),
            ..Default::default()
        };
        let raw = String::from_utf8(build(&o).unwrap().formatted()).unwrap();
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("Content-Type: text/calendar; charset=utf-8; method=REPLY"));
        assert!(raw.contains("METHOD:REPLY"));
    }

    use super::*;

    #[test]
    fn builds_a_threaded_reply_with_both_bodies() {
        let o = Outgoing {
            from: "me@example.com".into(),
            from_name: Some("Me".into()),
            to: split_addresses("Alice <alice@example.org>; bob@example.org"),
            subject: "Re: Hello".into(),
            html: "<p>Thanks <b>Alice</b></p>".into(),
            text: "Thanks Alice".into(),
            in_reply_to: Some("abc@example.org".into()),
            references: vec!["root@example.org".into(), "abc@example.org".into()],
            message_id: Some("draft-1@example.com".into()),
            ..Default::default()
        };
        let raw = String::from_utf8(build(&o).unwrap().formatted()).unwrap();
        assert!(raw.contains("In-Reply-To: <abc@example.org>"), "{raw}");
        assert!(raw.contains("References: <root@example.org> <abc@example.org>"));
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("text/html") && raw.contains("text/plain"));
        assert!(raw.contains("To: Alice <alice@example.org>, bob@example.org"));
        assert!(raw.contains("Message-ID: <draft-1@example.com>"), "{raw}");
        assert!(build(&Outgoing { from: "me@example.com".into(), ..Default::default() }).is_err());
        // Plain text only: one text part, no HTML.
        let plain = String::from_utf8(
            build(&Outgoing { from: "me@example.com".into(), to: vec!["a@example.org".into()], text: "Just text".into(), ..Default::default() })
                .unwrap()
                .formatted(),
        )
        .unwrap();
        assert!(plain.contains("Content-Type: text/plain") && !plain.contains("text/html") && plain.contains("Just text"), "{plain}");
        // A draft with nobody to send to yet still builds.
        let d = String::from_utf8(
            build_draft(&Outgoing { from: "me@example.com".into(), subject: "half written".into(), ..Default::default() }).unwrap().formatted(),
        )
        .unwrap();
        assert!(d.contains("Subject: half written") && !d.contains("\nTo:"), "{d}");
        assert!(!plain.contains("Auto-Submitted"));
        let auto =
            Outgoing { from: "me@example.com".into(), to: vec!["a@example.org".into()], text: "Away".into(), auto_reply: true, ..Default::default() };
        let auto = String::from_utf8(build(&auto).unwrap().formatted()).unwrap();
        assert!(auto.contains("Auto-Submitted: auto-replied"), "{auto}");
        let id = Outgoing {
            from: "me@example.com".into(),
            to: vec!["a@example.org".into()],
            text: "Hi".into(),
            reply_to: Some("office@example.com".into()),
            organisation: Some("Perkin Prints".into()),
            ..Default::default()
        };
        let id = String::from_utf8(build(&id).unwrap().formatted()).unwrap();
        assert!(id.contains("Reply-To: office@example.com") && id.contains("Organization: Perkin Prints"), "{id}");
    }
}
