//! The local full-text index (SQLite FTS5), so search finds mail by what it says without
//! asking the server: every message's subject, people and categories from the moment it
//! syncs, and the text of its body and its attachment names once it is downloaded.
//! `message.indexed` says how far a message has got: 0 not yet, 1 headers, 2 with its body. Bodies are decoded first: base64 and quoted-printable parts,
//! and HTML turned into its text, so a word matches however the message was sent.

use mail_parser::{MessageParser, MimeHeaders};
use rusqlite::{Connection, params};

/// How much of one message's text is indexed (a mailing-list digest can be megabytes).
const MAX_BODY: usize = 256 * 1024;

pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5(
           subject, people, body, attachments, categories, tokenize = 'unicode61 remove_diacritics 2');
         CREATE TRIGGER IF NOT EXISTS message_fts_gone AFTER DELETE ON message BEGIN
           DELETE FROM message_fts WHERE rowid = old.id;
         END;",
    )
}

/// What of a message goes in the index.
#[derive(Debug, Default, PartialEq)]
pub struct Text {
    pub subject: String,
    pub people: String,
    pub body: String,
    pub attachments: String,
}

pub fn extract(raw: &[u8]) -> Text {
    let Some(m) = MessageParser::default().parse(raw) else { return Text::default() };
    let mut people = vec![];
    for list in [m.from(), m.to(), m.cc(), m.reply_to()].into_iter().flatten() {
        for a in list.iter() {
            people.extend(a.name().map(str::to_string));
            people.extend(a.address().map(str::to_string));
        }
    }
    // body_text gives plain text, made from the HTML when that is all there is.
    let mut body = String::new();
    for i in 0..m.text_body.len() {
        if body.len() >= MAX_BODY {
            break;
        }
        if let Some(t) = m.body_text(i) {
            body.push_str(&t);
            body.push('\n');
        }
    }
    if body.len() > MAX_BODY {
        let mut end = MAX_BODY;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
    }
    let attachments: Vec<String> = m.attachments().filter_map(|a| a.attachment_name().map(str::to_string)).collect();
    Text { subject: m.subject().unwrap_or_default().to_string(), people: people.join(" "), body, attachments: attachments.join(" ") }
}

/// What a message is found by as categories: the ones kept here, and the IMAP keywords on it
/// (categories set from another PC arrive that way). Not system flags (\\Seen) or the
/// keywords mail programs keep for themselves ($Forwarded, Junk...).
pub fn category_words(categories: &str, flags: &str) -> String {
    let own = |k: &&str| !k.starts_with('\\') && !k.starts_with('$') && !["junk", "nonjunk", "notjunk"].contains(&k.to_ascii_lowercase().as_str());
    let mut words: Vec<&str> = categories.split_whitespace().collect();
    for k in flags.split_whitespace().filter(own) {
        if !words.iter().any(|w| w.eq_ignore_ascii_case(k)) {
            words.push(k);
        }
    }
    words.join(" ")
}

fn categories_of(db: &Connection, id: i64) -> rusqlite::Result<String> {
    db.query_row("SELECT categories, flags FROM message WHERE id = ?1", [id], |r| {
        Ok(category_words(&r.get::<_, String>(0)?, &r.get::<_, String>(1)?))
    })
}

/// (Re)index one downloaded message; `id` is its row in `message`.
pub(crate) fn index(db: &Connection, id: i64, raw: &[u8]) -> rusqlite::Result<()> {
    let t = extract(raw);
    let cats = categories_of(db, id)?;
    db.execute("DELETE FROM message_fts WHERE rowid = ?1", [id])?;
    db.execute(
        // The cached headers (what the list shows), and what the message itself adds.
        "INSERT INTO message_fts(rowid, subject, people, body, attachments, categories)
         SELECT ?1, CASE WHEN COALESCE(subject, '') = '' THEN ?2 ELSE subject END,
           COALESCE(from_name, '') || ' ' || COALESCE(from_addr, '') || ' ' || COALESCE(to_addrs, '') || ' ' || ?3, ?4, ?5, ?6
         FROM message WHERE id = ?1",
        params![id, t.subject, t.people, t.body, t.attachments, cats],
    )?;
    db.execute("UPDATE message SET indexed = 2 WHERE id = ?1", [id])?;
    Ok(())
}

/// Messages not in the index at all, by their headers (the cache has those for every
/// message); at most `limit`. How many.
pub(crate) fn index_headers(db: &Connection, account: &str, limit: usize) -> rusqlite::Result<usize> {
    let ids: Vec<i64> = db
        .prepare("SELECT m.id FROM message m JOIN folder f ON f.id = m.folder_id WHERE f.account = ?1 AND m.indexed = 0 LIMIT ?2")?
        .query_map(params![account, limit as i64], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for id in &ids {
        index_one_header(db, *id)?;
    }
    Ok(ids.len())
}

pub(crate) fn index_one_header(db: &Connection, id: i64) -> rusqlite::Result<()> {
    let cats = categories_of(db, id)?;
    db.execute(
        "INSERT INTO message_fts(rowid, subject, people, body, attachments, categories)
         SELECT id, COALESCE(subject, ''), COALESCE(from_name, '') || ' ' || COALESCE(from_addr, '') || ' ' || COALESCE(to_addrs, ''), '', '', ?2
         FROM message WHERE id = ?1 AND indexed = 0",
        params![id, cats],
    )?;
    db.execute("UPDATE message SET indexed = 1 WHERE id = ?1 AND indexed = 0", [id])?;
    Ok(())
}

/// A message's categories or keywords changed: so does what search finds it by.
pub(crate) fn refresh_categories(db: &Connection, id: i64) -> rusqlite::Result<()> {
    let cats = categories_of(db, id)?;
    db.execute("UPDATE message_fts SET categories = ?2 WHERE rowid = ?1", params![id, cats])?;
    Ok(())
}

/// One search word as an FTS5 query: a quoted string (so punctuation and FTS operators in it
/// are just text), matching words that start with it.
pub(crate) fn query(word: &str) -> String {
    format!("\"{}\"*", word.replace('"', "\"\""))
}

/// Every word, each as `query` does it. Single letters are left out: "e*" is half the
/// dictionary, and slow; a search of only those has no query.
pub(crate) fn query_all(text: &str) -> Option<String> {
    let words: Vec<String> = text.split_whitespace().filter(|w| w.chars().count() >= 2).take(8).map(query).collect();
    (!words.is_empty()).then(|| words.join(" AND "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_before_indexing() {
        // An HTML-only body in base64: the raw message has none of its words.
        let html = "<html><body><p>Your <b>filament</b> order has shipped</p><style>p{}</style></body></html>";
        let raw = format!(
            "From: Bambu Lab <store@example.com>\r\nTo: Mark <mark@example.com>\r\nSubject: Order update\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n\
             --b\r\nContent-Type: application/pdf; name=\"invoice-2231.pdf\"\r\nContent-Disposition: attachment; filename=\"invoice-2231.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\nJVBERi0=\r\n--b--\r\n",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, html)
        );
        assert!(!raw.contains("filament"));
        let t = extract(raw.as_bytes());
        assert_eq!(t.subject, "Order update");
        assert!(t.body.contains("filament"), "{:?}", t.body);
        assert!(t.people.contains("store@example.com") && t.people.contains("Bambu Lab"));
        assert_eq!(t.attachments, "invoice-2231.pdf");
        let _ = extract(b"not a message at all");
        let _ = extract(b"");
    }

    #[test]
    fn query_words_are_literal() {
        assert_eq!(query("filament"), "\"filament\"*");
        assert_eq!(query("say \"hi\" OR"), "\"say \"\"hi\"\" OR\"*");
        assert_eq!(query_all("a filament  spool"), Some("\"filament\"* AND \"spool\"*".into()));
        assert_eq!(query_all("e"), None);
        assert_eq!(category_words("Bills", "\\Seen $Forwarded Junk Receipts bills"), "Bills Receipts");
    }
}
