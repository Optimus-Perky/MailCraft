//! Bringing the local cache up to date with the server, and fetching whole messages on demand.

use crate::Error;
use crate::connect::Session;
use crate::store::{Header, Store};
use futures::TryStreamExt;
use serde::Serialize;

#[cfg(test)]
mod tests {
    #[test]
    fn dates_are_utc_so_they_sort() {
        let a = super::parse_header(1, String::new(), None, None, b"Date: Thu, 08 Oct 2026 20:39:20 +0100\r\nSubject: a\r\n\r\n");
        let b = super::parse_header(2, String::new(), None, None, b"Date: Thu, 08 Oct 2026 20:17:47 +0000\r\nSubject: b\r\n\r\n");
        assert_eq!(a.date.as_deref(), Some("2026-10-08T19:39:20Z"));
        assert!(b.date > a.date, "{:?} should sort after {:?}", b.date, a.date);
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub folders: usize,
    pub new_messages: usize,
    pub removed: usize,
    pub reset_folders: Vec<String>,
    /// Reading message outlines (attachments, previews) stopped early: why.
    pub outline_error: Option<String>,
}

fn flag_text(f: &async_imap::types::Flag<'_>) -> String {
    use async_imap::types::Flag;
    match f {
        Flag::Seen => "\\Seen".into(),
        Flag::Answered => "\\Answered".into(),
        Flag::Flagged => "\\Flagged".into(),
        Flag::Deleted => "\\Deleted".into(),
        Flag::Draft => "\\Draft".into(),
        Flag::Recent => "\\Recent".into(),
        Flag::MayCreate => "\\*".into(),
        Flag::Custom(c) => c.to_string(),
    }
}

/// RFC 6154 special use, read from the LIST attributes; INBOX is always the inbox.
fn special_use(name: &str, attrs: &[async_imap::types::NameAttribute<'_>]) -> Option<String> {
    if name.eq_ignore_ascii_case("INBOX") {
        return Some("\\Inbox".into());
    }
    for a in attrs {
        let t = format!("{a:?}");
        for s in ["Sent", "Drafts", "Trash", "Junk", "Archive", "All", "Flagged"] {
            if t == s || t.contains(&format!("\"\\\\{s}\"")) {
                return Some(format!("\\{s}"));
            }
        }
    }
    None
}

/// A Unix time as RFC 3339 in UTC ("2026-10-08T19:39:20Z").
pub fn utc(ts: i64) -> String {
    mail_parser::DateTime::from_timestamp(ts).to_rfc3339()
}

/// Parse the headers of a message (just its header block is enough).
pub fn parse_header(uid: u32, flags: String, internal_date: Option<String>, size: Option<u32>, raw: &[u8]) -> Header {
    let mut h = Header { uid, flags, internal_date, size, ..Default::default() };
    if let Some(m) = mail_parser::MessageParser::default().parse(raw) {
        h.subject = m.subject().map(str::to_string);
        if let Some(a) = m.from().and_then(|a| a.first()) {
            h.from_name = a.name().map(str::to_string);
            h.from_addr = a.address().map(str::to_string);
        }
        h.to = m.to().map(|list| list.iter().filter_map(|a| a.address().map(str::to_string)).collect::<Vec<_>>().join(", "));
        // Stored in UTC, so newest-first sorting works whatever zone the sender wrote.
        h.date = m.date().map(|d| utc(d.to_timestamp()));
        h.message_id = m.message_id().map(str::to_string);
        h.in_reply_to = m.in_reply_to().as_text().map(str::to_string);
    }
    h.bulk = crate::outline::is_bulk(raw);
    h.priority = crate::outline::priority(raw);
    h
}

/// Sync every selectable folder of one account into the store.
pub async fn sync_account(session: &mut Session, store: &Store, account: &str) -> Result<Report, Error> {
    let mut report = Report::default();
    let names: Vec<_> = session.list(Some(""), Some("*")).await?.try_collect().await?;
    let mut live = vec![];
    let mut folders = vec![];
    for n in &names {
        let attrs = n.attributes();
        if attrs.iter().any(|a| format!("{a:?}") == "NoSelect") {
            continue;
        }
        let name = n.name().to_string();
        let id = store.folder(account, &name, n.delimiter(), special_use(&name, attrs).as_deref())?;
        live.push(name.clone());
        folders.push((id, name));
    }
    store.prune_folders(account, &live)?;
    for (id, name) in folders {
        sync_folder(session, store, id, &name, &mut report).await?;
    }
    Ok(report)
}

/// Bring just these folders up to date: what a delete or move touched, so the folder tree
/// shows it without checking every folder in the account (2026-10-29: 56 folders took
/// 2.5 s after every delete). A folder the cache does not know is left to the next full sync.
pub async fn sync_folders(session: &mut Session, store: &Store, account: &str, names: &[String]) -> Result<Report, Error> {
    let mut report = Report::default();
    for name in names {
        if let Some(id) = store.folder_id(account, name)? {
            sync_folder(session, store, id, name, &mut report).await?;
        }
    }
    Ok(report)
}

async fn sync_folder(session: &mut Session, store: &Store, id: i64, name: &str, report: &mut Report) -> Result<(), Error> {
    report.folders += 1;
    let mb = session.select(name).await?;
    let (cached_validity, max_uid) = store.sync_state(id)?;
    let validity = mb.uid_validity.unwrap_or(0);
    let mut max_uid = max_uid;
    if cached_validity != Some(validity) {
        if cached_validity.is_some() {
            report.reset_folders.push(name.to_string());
        }
        store.reset_folder(id, validity)?;
        max_uid = 0;
    }
    if mb.exists == 0 {
        report.removed += store.keep_only(id, &[])?;
        return Ok(());
    }
    // Which UIDs still exist: anything cached but not listed was deleted or moved elsewhere.
    let uids: Vec<u32> = session.uid_search("ALL").await?.into_iter().collect();
    report.removed += store.keep_only(id, &uids)?;
    // Flags on what we already have (read/unread, flagged, answered).
    if max_uid > 0 {
        let fetched: Vec<_> = session.uid_fetch(format!("1:{max_uid}"), "(UID FLAGS)").await?.try_collect().await?;
        for f in &fetched {
            if let Some(uid) = f.uid {
                store.set_flags(id, uid, &f.flags().map(|x| flag_text(&x)).collect::<Vec<_>>().join(" "))?;
            }
        }
    }
    // New messages: headers only here; bodies come when opened, or from the background
    // download (Settings > Storage).
    if uids.iter().any(|&u| u > max_uid) {
        let fetched: Vec<_> =
            session.uid_fetch(format!("{}:*", max_uid + 1), "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER])").await?.try_collect().await?;
        for f in &fetched {
            let Some(uid) = f.uid else { continue };
            if uid <= max_uid {
                continue; // "n:*" always returns the last message, even an old one
            }
            let flags = f.flags().map(|x| flag_text(&x)).collect::<Vec<_>>().join(" ");
            let h = parse_header(uid, flags, f.internal_date().map(|d| utc(d.timestamp())), f.size, f.header().unwrap_or_default());
            store.put_header(id, &h)?;
            report.new_messages += 1;
        }
    }
    // Outlines are extras: a server that answers them badly must not stop the mail itself.
    if report.outline_error.is_none()
        && let Err(e) = outline_folder(session, store, id).await
    {
        report.outline_error = Some(e.to_string());
    }
    Ok(())
}

/// Read the structure of messages not yet outlined (newest first, a batch per sync, so a big
/// mailbox fills in over a few syncs): attachment names, bulk mail, and a preview of the newest.
async fn outline_folder(session: &mut Session, store: &Store, id: i64) -> Result<(), Error> {
    use crate::outline;
    use async_imap::imap_proto::{MessageSection, SectionPath};
    const BATCH: usize = 500;
    const PREVIEWS: usize = 200;
    let todo = store.needs_outline(id, BATCH)?;
    if todo.is_empty() {
        return Ok(());
    }
    let set = todo.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let fetched: Vec<_> = session
        .uid_fetch(
            &set,
            "(UID BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS (FROM LIST-UNSUBSCRIBE LIST-ID PRECEDENCE AUTO-SUBMITTED X-PRIORITY IMPORTANCE PRIORITY)])",
        )
        .await?
        .try_collect()
        .await?;
    let mut texts: Vec<(u32, outline::TextPart)> = vec![];
    for f in &fetched {
        let Some(uid) = f.uid else { continue };
        let o = f.bodystructure().map(outline::outline).unwrap_or_default();
        let head = f.section(&SectionPath::Full(MessageSection::Header)).or(f.header());
        let bulk = head.is_some_and(outline::is_bulk);
        store.set_outline(id, uid, &o.attachments, bulk, head.and_then(outline::priority))?;
        if let Some(t) = o.text {
            texts.push((uid, t));
        }
    }
    // Previews for the newest only; older ones get theirs when opened.
    texts.sort_by_key(|(u, _)| std::cmp::Reverse(*u));
    texts.truncate(PREVIEWS);
    let want = store.without_preview(id, &texts.iter().map(|(u, _)| *u).collect::<Vec<_>>())?;
    texts.retain(|(u, _)| want.contains(u));
    // Marked as tried first: one that fails is not asked for again on every sync.
    for (u, _) in &texts {
        store.set_preview(id, *u, "")?;
    }
    // One FETCH per part number and length (most messages share one: 1, or 1.1). HTML spends
    // its first bytes on styles, so it needs more to reach the words. A part no longer than
    // that is fetched whole.
    let mut by_part: std::collections::BTreeMap<(String, Option<u32>), Vec<(u32, outline::TextPart)>> = Default::default();
    for (u, t) in texts {
        let want = if t.html { 6000 } else { 1500 };
        let partial = (t.size == 0 || t.size > want).then_some(want);
        by_part.entry((t.section_text(), partial)).or_default().push((u, t));
    }
    for ((section, partial), group) in by_part {
        let set = group.iter().map(|(u, _)| u.to_string()).collect::<Vec<_>>().join(",");
        let what = match partial {
            Some(n) => format!("(UID BODY.PEEK[{section}]<0.{n}>)"),
            None => format!("(UID BODY.PEEK[{section}])"),
        };
        let fetched: Vec<_> = session.uid_fetch(&set, what).await?.try_collect().await?;
        let path = SectionPath::Part(group.first().map(|(_, t)| t.section.clone()).unwrap_or_default(), None);
        for f in &fetched {
            let Some(uid) = f.uid else { continue };
            let (Some(data), Some((_, part))) = (f.section(&path), group.iter().find(|(u, _)| *u == uid)) else { continue };
            store.set_preview(id, uid, &outline::preview_from_part(part, data))?;
        }
    }
    Ok(())
}

/// Download whole messages of one folder (without marking them read) into the cache, where
/// they are indexed for search; how many arrived. UIDs gone from the server are skipped.
pub async fn fetch_bodies(session: &mut Session, store: &Store, account: &str, folder: &str, uids: &[u32]) -> Result<usize, Error> {
    let Some(id) = store.folder_id(account, folder)? else { return Ok(0) };
    if uids.is_empty() {
        return Ok(0);
    }
    session.select(folder).await?;
    let set = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let mut stream = session.uid_fetch(set, "(UID BODY.PEEK[])").await?;
    let mut n = 0;
    while let Some(f) = stream.try_next().await? {
        if let (Some(uid), Some(raw)) = (f.uid, f.body()) {
            store.put_body(id, uid, raw)?;
            store.set_preview(id, uid, &crate::outline::preview_from_message(raw))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Fetch one whole message (without marking it read) and keep it in the cache.
pub async fn fetch_body(session: &mut Session, store: &Store, account: &str, folder: &str, uid: u32) -> Result<Vec<u8>, Error> {
    let id = store.folder_id(account, folder)?.ok_or_else(|| Error::Imap(format!("folder {folder} is not in the cache; sync first")))?;
    if let Some(raw) = store.body(id, uid)? {
        return Ok(raw);
    }
    session.select(folder).await?;
    let fetched: Vec<_> = session.uid_fetch(uid.to_string(), "(UID BODY.PEEK[])").await?.try_collect().await?;
    let raw = fetched
        .iter()
        .find(|f| f.uid == Some(uid))
        .and_then(|f| f.body())
        .map(<[u8]>::to_vec)
        .ok_or_else(|| Error::Imap(format!("no message with UID {uid} in {folder}")))?;
    store.put_body(id, uid, &raw)?;
    // The whole message is the best source for its preview.
    store.set_preview(id, uid, &crate::outline::preview_from_message(&raw))?;
    Ok(raw)
}
