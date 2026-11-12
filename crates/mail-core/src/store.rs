//! The local mail cache: one SQLite file holding folders, message headers and fetched bodies.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::path::Path;

pub struct Store {
    pub(crate) db: Connection,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderRow {
    pub id: i64,
    pub account: String,
    pub name: String,
    pub delimiter: Option<String>,
    /// RFC 6154 special use: \Inbox (ours), \Sent, \Drafts, \Trash, \Junk, \Archive, \All, \Flagged.
    pub special: Option<String>,
    pub uidvalidity: Option<u32>,
    pub total: i64,
    pub unread: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MessageRow {
    pub id: i64,
    pub uid: u32,
    pub flags: String,
    pub date: Option<String>,
    pub from_name: Option<String>,
    pub from_addr: Option<String>,
    pub to: Option<String>,
    pub subject: Option<String>,
    pub size: Option<u32>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub has_body: bool,
    /// The folder it is in (lists can span folders: searching them all).
    pub folder: String,
    /// Category keywords set here (the server's own keywords are in `flags`).
    pub categories: String,
    /// Attachment names; None until the sync has read the message's structure.
    pub attachments: Option<Vec<String>>,
    /// The first words of its text, when fetched.
    pub preview: Option<String>,
    /// Newsletters, lists and notifications (the Focused Inbox puts them in Other).
    pub bulk: bool,
    /// When it arrived (IMAP INTERNALDATE), as against `date`, when it was sent.
    pub received: Option<String>,
    /// 1 (highest) to 5 (lowest), when the sender set one (X-Priority, Importance).
    pub priority: Option<u8>,
}

impl MessageRow {
    pub fn seen(&self) -> bool {
        self.flags.split(' ').any(|f| f.eq_ignore_ascii_case("\\Seen"))
    }
}

/// A message in Trash and the folder it was deleted from, when MailCraft saw it go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub uid: u32,
    pub message_id: Option<String>,
    pub folder: Option<String>,
}

/// What a sync learnt about one message's header.
#[derive(Debug, Clone, Default)]
pub struct Header {
    pub uid: u32,
    pub flags: String,
    pub internal_date: Option<String>,
    pub size: Option<u32>,
    pub date: Option<String>,
    pub from_name: Option<String>,
    pub from_addr: Option<String>,
    pub to: Option<String>,
    pub subject: Option<String>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub bulk: bool,
    pub priority: Option<u8>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let db = Connection::open(path)?;
        // The background download and server search have connections of their own: wait for
        // each other's writes rather than fail with "database is locked".
        db.busy_timeout(std::time::Duration::from_secs(15))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS folder (
               id INTEGER PRIMARY KEY, account TEXT NOT NULL, name TEXT NOT NULL, delimiter TEXT,
               special TEXT, uidvalidity INTEGER, uidnext INTEGER, UNIQUE(account, name));
             CREATE TABLE IF NOT EXISTS message (
               id INTEGER PRIMARY KEY, folder_id INTEGER NOT NULL REFERENCES folder(id) ON DELETE CASCADE,
               uid INTEGER NOT NULL, flags TEXT NOT NULL DEFAULT '', internal_date TEXT, size INTEGER,
               date TEXT, from_name TEXT, from_addr TEXT, to_addrs TEXT, subject TEXT,
               message_id TEXT, in_reply_to TEXT, raw BLOB, UNIQUE(folder_id, uid));
             CREATE INDEX IF NOT EXISTS message_by_date ON message(folder_id, date DESC);",
        )?;
        // Added later: categories MailCraft keeps itself (servers that refuse keywords, and so a
        // sync, which rewrites the flags, never loses them). Already there on a newer cache.
        let has: bool = db.prepare("SELECT 1 FROM pragma_table_info('message') WHERE name = 'categories'")?.exists([])?;
        if !has {
            db.execute_batch("ALTER TABLE message ADD COLUMN categories TEXT NOT NULL DEFAULT ''")?;
        }
        // Added later: what the list shows without the whole message (outline.rs). NULL
        // attachments means the message's structure has not been read yet.
        for (col, ty) in [("attachments", "TEXT"), ("preview", "TEXT"), ("bulk", "INTEGER NOT NULL DEFAULT 0"), ("priority", "INTEGER")] {
            let has: bool = db.prepare(&format!("SELECT 1 FROM pragma_table_info('message') WHERE name = '{col}'"))?.exists([])?;
            if !has {
                db.execute_batch(&format!("ALTER TABLE message ADD COLUMN {col} {ty}"))?;
                // Priority came later: read every message's outline again (in the background,
                // a batch a sync) so older mail gets one too.
                if col == "priority" {
                    db.execute_batch("UPDATE message SET attachments = NULL")?;
                }
            }
        }
        // Previews made before HTML style sheets were left out could be CSS ("html, body {"):
        // make them again (once).
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            db.execute_batch("UPDATE message SET preview = NULL, attachments = NULL WHERE preview LIKE '%{%'; PRAGMA user_version = 1;")?;
        }
        // Categories kept here are also remembered by Message-ID, so a message moved or copied
        // to another folder (where it gets a new UID) has them again when that folder syncs.
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS kept_category (
               account TEXT NOT NULL, message_id TEXT NOT NULL, categories TEXT NOT NULL,
               PRIMARY KEY(account, message_id));
             INSERT OR IGNORE INTO kept_category(account, message_id, categories)
               SELECT f.account, m.message_id, m.categories FROM message m JOIN folder f ON f.id = m.folder_id
               WHERE m.categories != '' AND m.message_id IS NOT NULL AND m.message_id != '';",
        )?;
        // Where a message deleted here (moved to Trash) came from, so Restore can put it back.
        // By Message-ID, as kept categories are: the copy in Trash has a UID of its own.
        // `deleted` is a Unix time, for forgetting messages Trash lost some other way.
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS trash_origin (
               account TEXT NOT NULL, message_id TEXT NOT NULL, folder TEXT NOT NULL, deleted INTEGER NOT NULL,
               PRIMARY KEY(account, message_id));",
        )?;
        // Downloaded messages live in their own table, so the message rows (which every list
        // and search reads) stay small however much mail is kept here; body_size says whether
        // a message is downloaded. Bodies kept in message.raw by older versions move across.
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS body (id INTEGER PRIMARY KEY, raw BLOB NOT NULL);
             CREATE TRIGGER IF NOT EXISTS body_gone AFTER DELETE ON message BEGIN
               DELETE FROM body WHERE id = old.id;
             END;",
        )?;
        // Search's indexes: newest first across an account, and what is left to index.
        let has: bool = db.prepare("SELECT 1 FROM pragma_table_info('message') WHERE name = 'indexed'")?.exists([])?;
        if !has {
            db.execute_batch("ALTER TABLE message ADD COLUMN indexed INTEGER NOT NULL DEFAULT 0")?;
        }
        db.execute_batch(
            "CREATE INDEX IF NOT EXISTS message_by_sort ON message(COALESCE(date, internal_date));
             CREATE INDEX IF NOT EXISTS message_to_index ON message(folder_id) WHERE indexed = 0;",
        )?;
        let has: bool = db.prepare("SELECT 1 FROM pragma_table_info('message') WHERE name = 'body_size'")?.exists([])?;
        if !has {
            db.execute_batch(
                "BEGIN;
                 ALTER TABLE message ADD COLUMN body_size INTEGER;
                 INSERT OR REPLACE INTO body(id, raw) SELECT id, raw FROM message WHERE raw IS NOT NULL;
                 UPDATE message SET body_size = LENGTH(raw), raw = NULL WHERE raw IS NOT NULL;
                 COMMIT;",
            )?;
        }
        crate::calstore::init(&db)?;
        crate::fulltext::init(&db)?;
        Ok(Store { db })
    }

    pub fn folder(&self, account: &str, name: &str, delimiter: Option<&str>, special: Option<&str>) -> rusqlite::Result<i64> {
        self.db.execute(
            "INSERT INTO folder(account, name, delimiter, special) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(account, name) DO UPDATE SET delimiter = excluded.delimiter, special = excluded.special",
            params![account, name, delimiter, special],
        )?;
        self.db.query_row("SELECT id FROM folder WHERE account = ?1 AND name = ?2", params![account, name], |r| r.get(0))
    }

    /// Forget folders the server no longer has.
    pub fn prune_folders(&self, account: &str, keep: &[String]) -> rusqlite::Result<usize> {
        let mut gone = 0;
        let names: Vec<String> =
            self.db.prepare("SELECT name FROM folder WHERE account = ?1")?.query_map([account], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for n in names.iter().filter(|n| !keep.contains(n)) {
            gone += self.db.execute("DELETE FROM folder WHERE account = ?1 AND name = ?2", params![account, n])?;
        }
        Ok(gone)
    }

    /// The folder's UIDVALIDITY and highest cached UID.
    pub fn sync_state(&self, folder_id: i64) -> rusqlite::Result<(Option<u32>, u32)> {
        let v: Option<u32> = self.db.query_row("SELECT uidvalidity FROM folder WHERE id = ?1", [folder_id], |r| r.get(0))?;
        let max: Option<u32> = self.db.query_row("SELECT MAX(uid) FROM message WHERE folder_id = ?1", [folder_id], |r| r.get(0))?;
        Ok((v, max.unwrap_or(0)))
    }

    /// A new UIDVALIDITY means every cached UID is meaningless: drop them.
    pub fn reset_folder(&self, folder_id: i64, uidvalidity: u32) -> rusqlite::Result<()> {
        self.db.execute("DELETE FROM message WHERE folder_id = ?1", [folder_id])?;
        self.db.execute("UPDATE folder SET uidvalidity = ?2 WHERE id = ?1", params![folder_id, uidvalidity])?;
        Ok(())
    }

    pub fn put_header(&self, folder_id: i64, h: &Header) -> rusqlite::Result<()> {
        let (id, indexed): (i64, i64) = self.db.query_row(
            "INSERT INTO message(folder_id, uid, flags, internal_date, size, date, from_name, from_addr, to_addrs, subject, message_id, in_reply_to, bulk, priority, categories)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
               COALESCE((SELECT k.categories FROM kept_category k JOIN folder f ON f.account = k.account
                         WHERE f.id = ?1 AND k.message_id = ?11), ''))
             ON CONFLICT(folder_id, uid) DO UPDATE SET flags = excluded.flags
             RETURNING id, indexed",
            params![
                folder_id,
                h.uid,
                h.flags,
                h.internal_date,
                h.size,
                h.date,
                h.from_name,
                h.from_addr,
                h.to,
                h.subject,
                h.message_id,
                h.in_reply_to,
                h.bulk,
                h.priority
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        // Searchable by its headers at once (by its words once downloaded).
        if indexed == 0 {
            crate::fulltext::index_one_header(&self.db, id)?;
        }
        Ok(())
    }

    /// Add one flag to a cached message (if it is not there already).
    pub fn add_flag(&self, folder_id: i64, uid: u32, flag: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "UPDATE message SET flags = TRIM(flags || ' ' || ?3) WHERE folder_id = ?1 AND uid = ?2 AND ' ' || flags || ' ' NOT LIKE '% ' || ?3 || ' %'",
            params![folder_id, uid, flag],
        )?;
        Ok(())
    }

    /// Turn one flag on or off on a cached message, keeping its others: the screen changes at
    /// once and the server is told after.
    pub fn change_flag(&self, folder_id: i64, uid: u32, flag: &str, on: bool) -> rusqlite::Result<()> {
        let flags: Option<String> =
            self.db.query_row("SELECT flags FROM message WHERE folder_id = ?1 AND uid = ?2", params![folder_id, uid], |r| r.get(0)).optional()?;
        let Some(flags) = flags else { return Ok(()) };
        let mut list: Vec<&str> = flags.split_whitespace().filter(|f| !f.eq_ignore_ascii_case(flag)).collect();
        if on {
            list.push(flag);
        }
        self.set_flags(folder_id, uid, &list.join(" "))
    }

    /// A message's flags as the server has them now. Written only when they changed (every
    /// sync sends them all); a keyword added or taken off elsewhere (a category set on another
    /// PC) changes what search finds the message by.
    pub fn set_flags(&self, folder_id: i64, uid: u32, flags: &str) -> rusqlite::Result<()> {
        let changed: Option<i64> = self
            .db
            .query_row(
                "UPDATE message SET flags = ?3 WHERE folder_id = ?1 AND uid = ?2 AND flags IS NOT ?3 RETURNING id",
                params![folder_id, uid, flags],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = changed {
            crate::fulltext::refresh_categories(&self.db, id)?;
        }
        Ok(())
    }

    /// Remove cached messages whose UIDs the server no longer lists.
    pub fn keep_only(&self, folder_id: i64, uids: &[u32]) -> rusqlite::Result<usize> {
        let cached: Vec<u32> = self
            .db
            .prepare("SELECT uid FROM message WHERE folder_id = ?1")?
            .query_map([folder_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let live: std::collections::HashSet<u32> = uids.iter().copied().collect();
        let mut n = 0;
        for u in cached.into_iter().filter(|u| !live.contains(u)) {
            n += self.db.execute("DELETE FROM message WHERE folder_id = ?1 AND uid = ?2", params![folder_id, u])?;
        }
        Ok(n)
    }

    pub fn remove_flag(&self, folder_id: i64, uid: u32, flag: &str) -> rusqlite::Result<()> {
        let flags: Option<String> =
            self.db.query_row("SELECT flags FROM message WHERE folder_id = ?1 AND uid = ?2", params![folder_id, uid], |r| r.get(0)).optional()?;
        if let Some(f) = flags {
            let kept: Vec<&str> = f.split_whitespace().filter(|x| !x.eq_ignore_ascii_case(flag)).collect();
            self.set_flags(folder_id, uid, &kept.join(" "))?;
        }
        Ok(())
    }

    /// Forget messages that were moved or deleted (the next sync would too; this is at once).
    pub fn remove(&self, folder_id: i64, uids: &[u32]) -> rusqlite::Result<()> {
        for u in uids {
            self.db.execute("DELETE FROM message WHERE folder_id = ?1 AND uid = ?2", params![folder_id, u])?;
        }
        Ok(())
    }

    /// Remember the folder these messages are in, as they go to Trash (call it before taking
    /// them out of the cache). Messages without a Message-ID cannot be followed into Trash and
    /// are left out; how many were remembered.
    pub fn remember_origin(&self, folder_id: i64, uids: &[u32]) -> rusqlite::Result<usize> {
        let mut n = 0;
        for u in uids {
            n += self.db.execute(
                "INSERT INTO trash_origin(account, message_id, folder, deleted)
                 SELECT f.account, m.message_id, f.name, CAST(strftime('%s', 'now') AS INTEGER)
                 FROM message m JOIN folder f ON f.id = m.folder_id
                 WHERE m.folder_id = ?1 AND m.uid = ?2 AND m.message_id IS NOT NULL AND m.message_id != ''
                 ON CONFLICT(account, message_id) DO UPDATE SET folder = excluded.folder, deleted = excluded.deleted",
                params![folder_id, u],
            )?;
        }
        Ok(n)
    }

    /// The same for everything in a folder (emptying it into Trash).
    pub fn remember_folder_origin(&self, folder_id: i64) -> rusqlite::Result<usize> {
        self.db.execute(
            "INSERT INTO trash_origin(account, message_id, folder, deleted)
             SELECT f.account, m.message_id, f.name, CAST(strftime('%s', 'now') AS INTEGER)
             FROM message m JOIN folder f ON f.id = m.folder_id
             WHERE m.folder_id = ?1 AND m.message_id IS NOT NULL AND m.message_id != ''
             ON CONFLICT(account, message_id) DO UPDATE SET folder = excluded.folder, deleted = excluded.deleted",
            [folder_id],
        )
    }

    /// Messages in a folder (Trash) with where each was deleted from, when that is known.
    pub fn origins(&self, folder_id: i64, uids: &[u32]) -> rusqlite::Result<Vec<Origin>> {
        let mut st = self.db.prepare(
            "SELECT m.uid, m.message_id, o.folder FROM message m JOIN folder f ON f.id = m.folder_id
             LEFT JOIN trash_origin o ON o.account = f.account AND o.message_id = m.message_id
             WHERE m.folder_id = ?1 AND m.uid = ?2",
        )?;
        let mut out = vec![];
        for u in uids {
            if let Some(o) = st
                .query_row(params![folder_id, u], |r| {
                    Ok(Origin { uid: r.get(0)?, message_id: r.get::<_, Option<String>>(1)?.filter(|i| !i.is_empty()), folder: r.get(2)? })
                })
                .optional()?
            {
                out.push(o);
            }
        }
        Ok(out)
    }

    /// The Message-IDs of these messages (ones without one are left out), for following them
    /// into another folder, as Undo follows deleted mail into Trash.
    pub fn message_ids(&self, folder_id: i64, uids: &[u32]) -> rusqlite::Result<Vec<String>> {
        let mut st =
            self.db.prepare("SELECT message_id FROM message WHERE folder_id = ?1 AND uid = ?2 AND message_id IS NOT NULL AND message_id != ''")?;
        let mut out = vec![];
        for u in uids {
            if let Some(id) = st.query_row(params![folder_id, u], |r| r.get::<_, String>(0)).optional()? {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Where messages with these Message-IDs are in a folder, as (Message-ID, UID); one each,
    /// the newest arrival when a folder holds the same message twice (deleted again later).
    pub fn find_message_ids(&self, folder_id: i64, ids: &[String]) -> rusqlite::Result<Vec<(String, u32)>> {
        let mut st = self.db.prepare("SELECT MAX(uid) FROM message WHERE folder_id = ?1 AND message_id = ?2")?;
        let mut out = vec![];
        for id in ids {
            if let Some(u) = st.query_row(params![folder_id, id], |r| r.get::<_, Option<u32>>(0))? {
                out.push((id.clone(), u));
            }
        }
        Ok(out)
    }

    /// How many of these Message-IDs are in the account's other folders than `except` (Trash):
    /// mail an undo is waiting for that is back where it was (its delete failed).
    pub fn message_ids_elsewhere(&self, account: &str, except: i64, ids: &[String]) -> rusqlite::Result<usize> {
        let mut st = self.db.prepare(
            "SELECT EXISTS(SELECT 1 FROM message m JOIN folder f ON f.id = m.folder_id
             WHERE f.account = ?1 AND f.id != ?2 AND m.message_id = ?3)",
        )?;
        let mut n = 0;
        for id in ids {
            if st.query_row(params![account, except, id], |r| r.get::<_, bool>(0))? {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Forget where these messages came from (restored, or deleted for good).
    pub fn forget_origins(&self, account: &str, message_ids: &[String]) -> rusqlite::Result<usize> {
        let mut n = 0;
        for id in message_ids {
            n += self.db.execute("DELETE FROM trash_origin WHERE account = ?1 AND message_id = ?2", params![account, id])?;
        }
        Ok(n)
    }

    /// Forget where the messages in a folder came from: Trash emptied, or (`uids`) some of it
    /// deleted for good. Call it before they leave the cache.
    pub fn forget_origins_in(&self, folder_id: i64, uids: Option<&[u32]>) -> rusqlite::Result<usize> {
        const GONE: &str = "DELETE FROM trash_origin WHERE (account, message_id) IN
               (SELECT f.account, m.message_id FROM message m JOIN folder f ON f.id = m.folder_id WHERE m.folder_id = ?1";
        match uids {
            None => self.db.execute(&format!("{GONE})"), [folder_id]),
            Some(uids) => {
                let mut n = 0;
                for u in uids {
                    n += self.db.execute(&format!("{GONE} AND m.uid = ?2)"), params![folder_id, u])?;
                }
                Ok(n)
            }
        }
    }

    /// After a full sync: forget messages deleted more than `keep_secs` ago that are no longer
    /// anywhere in the account (Trash emptied by another app, or by the server's own clean-up).
    /// Younger ones stay, as a message just moved may not be in Trash's copy here yet.
    pub fn prune_origins(&self, account: &str, keep_secs: i64) -> rusqlite::Result<usize> {
        self.db.execute(
            "DELETE FROM trash_origin WHERE account = ?1 AND deleted < CAST(strftime('%s', 'now') AS INTEGER) - ?2
             AND message_id NOT IN (SELECT m.message_id FROM message m JOIN folder f ON f.id = m.folder_id
                                    WHERE f.account = ?1 AND m.message_id IS NOT NULL)",
            params![account, keep_secs],
        )
    }

    /// Cached messages in a folder whose sender, recipients or subject contain every word,
    /// or whose downloaded text does; newest first.
    pub fn search(&self, folder_id: i64, text: &str, limit: usize) -> rusqlite::Result<Vec<MessageRow>> {
        if text.trim().is_empty() {
            return self.messages(folder_id, limit);
        }
        self.search_where("folder_id = ?1", Box::new(folder_id), text, limit)
    }

    /// Every message of an account, newest first (search folders without words).
    pub fn messages_account(&self, account: &str, limit: usize) -> rusqlite::Result<Vec<MessageRow>> {
        let mut st =
            self.db.prepare(&format!("{SELECT_MESSAGE} WHERE f.account = ?1 ORDER BY COALESCE(date, internal_date) DESC, uid DESC LIMIT ?2"))?;
        st.query_map(params![account, limit as i64], row)?.collect()
    }

    /// The same, across every folder of an account.
    pub fn search_account(&self, account: &str, text: &str, limit: usize) -> rusqlite::Result<Vec<MessageRow>> {
        if text.trim().is_empty() {
            return Ok(vec![]);
        }
        self.search_where("f.account = ?1", Box::new(account.to_string()), text, limit)
    }

    fn search_where(&self, scope: &str, key: Box<dyn rusqlite::ToSql>, text: &str, limit: usize) -> rusqlite::Result<Vec<MessageRow>> {
        // The index: every message by its headers and categories, downloaded ones by their
        // words too. Across an account, walk newest first and stop at `limit`; a folder is
        // small enough to sort.
        if let Some(q) = crate::fulltext::query_all(text) {
            let from = if scope.starts_with("f.account") { "message m INDEXED BY message_by_sort" } else { "message m" };
            let sql = SELECT_MESSAGE.replacen("FROM message m", &format!("FROM {from}"), 1);
            let sql = format!(
                "{sql} WHERE {scope} AND m.id IN (SELECT rowid FROM message_fts WHERE message_fts MATCH ?3)
                 ORDER BY COALESCE(m.date, m.internal_date) DESC, m.uid DESC LIMIT ?2"
            );
            let mut st = self.db.prepare(&sql)?;
            let args: [Box<dyn rusqlite::ToSql>; 3] = [key, Box::new(limit as i64), Box::new(q)];
            return st.query_map(rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())), row)?.collect();
        }
        // Only single letters: by the headers.
        let words: Vec<String> =
            text.split_whitespace().take(8).map(|w| format!("%{}%", w.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))).collect();
        if words.is_empty() {
            return Ok(vec![]);
        }
        let cond = (0..words.len())
            .map(|i| {
                let p = i + 3;
                format!(
                    "(COALESCE(subject,'') LIKE ?{p} ESCAPE '\\' OR COALESCE(from_name,'') LIKE ?{p} ESCAPE '\\' OR COALESCE(from_addr,'') LIKE ?{p} ESCAPE '\\'
                      OR COALESCE(to_addrs,'') LIKE ?{p} ESCAPE '\\' OR m.categories LIKE ?{p} ESCAPE '\\')"
                )
            })
            .collect::<Vec<_>>()
            .join(" AND ");
        let sql = format!("{SELECT_MESSAGE} WHERE {scope} AND {cond} ORDER BY COALESCE(date, internal_date) DESC, uid DESC LIMIT ?2");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![key, Box::new(limit as i64)];
        for w in words {
            args.push(Box::new(w));
        }

        let mut st = self.db.prepare(&sql)?;
        st.query_map(rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())), row)?.collect()
    }

    /// These UIDs from the cache (for showing server search results); newest first.
    pub fn by_uids(&self, folder_id: i64, uids: &[u32]) -> rusqlite::Result<Vec<MessageRow>> {
        let set: std::collections::HashSet<u32> = uids.iter().copied().collect();
        Ok(self.messages(folder_id, usize::MAX >> 1)?.into_iter().filter(|m| set.contains(&m.uid)).collect())
    }

    /// Add or take off a category kept in the cache.
    pub fn set_category(&self, folder_id: i64, uid: u32, key: &str, on: bool) -> rusqlite::Result<()> {
        let now: Option<String> = self
            .db
            .query_row("SELECT categories FROM message WHERE folder_id = ?1 AND uid = ?2", params![folder_id, uid], |r| r.get(0))
            .optional()?;
        let Some(now) = now else { return Ok(()) };
        let mut keys: Vec<&str> = now.split_whitespace().filter(|k| *k != key).collect();
        if on {
            keys.push(key);
        }
        let keys = keys.join(" ");
        self.db.execute("UPDATE message SET categories = ?3 WHERE folder_id = ?1 AND uid = ?2", params![folder_id, uid, keys])?;
        let row: Option<i64> =
            self.db.query_row("SELECT id FROM message WHERE folder_id = ?1 AND uid = ?2", params![folder_id, uid], |r| r.get(0)).optional()?;
        if let Some(row) = row {
            crate::fulltext::refresh_categories(&self.db, row)?;
        }
        let (account, id): (String, Option<String>) = self.db.query_row(
            "SELECT f.account, m.message_id FROM message m JOIN folder f ON f.id = m.folder_id WHERE m.folder_id = ?1 AND m.uid = ?2",
            params![folder_id, uid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if let Some(id) = id.filter(|i| !i.is_empty()) {
            if keys.is_empty() {
                self.db.execute("DELETE FROM kept_category WHERE account = ?1 AND message_id = ?2", params![account, id])?;
            } else {
                self.db.execute(
                    "INSERT INTO kept_category(account, message_id, categories) VALUES (?1, ?2, ?3)
                     ON CONFLICT(account, message_id) DO UPDATE SET categories = excluded.categories",
                    params![account, id, keys],
                )?;
            }
        }
        Ok(())
    }

    /// Messages whose structure has not been read yet, newest first.
    pub fn needs_outline(&self, folder_id: i64, limit: usize) -> rusqlite::Result<Vec<u32>> {
        let mut st = self.db.prepare("SELECT uid FROM message WHERE folder_id = ?1 AND attachments IS NULL ORDER BY uid DESC LIMIT ?2")?;
        st.query_map(params![folder_id, limit as i64], |r| r.get(0))?.collect()
    }

    /// Of these UIDs, the ones with no preview yet.
    pub fn without_preview(&self, folder_id: i64, uids: &[u32]) -> rusqlite::Result<Vec<u32>> {
        let mut st = self.db.prepare("SELECT 1 FROM message WHERE folder_id = ?1 AND uid = ?2 AND preview IS NULL")?;
        let mut out = vec![];
        for u in uids {
            if st.exists(params![folder_id, u])? {
                out.push(*u);
            }
        }
        Ok(out)
    }

    pub fn set_outline(&self, folder_id: i64, uid: u32, attachments: &[String], bulk: bool, priority: Option<u8>) -> rusqlite::Result<()> {
        self.db.execute(
            "UPDATE message SET attachments = ?3, bulk = ?4, priority = COALESCE(?5, priority) WHERE folder_id = ?1 AND uid = ?2",
            params![folder_id, uid, attachments.join("\n"), bulk, priority],
        )?;
        Ok(())
    }

    pub fn set_preview(&self, folder_id: i64, uid: u32, preview: &str) -> rusqlite::Result<()> {
        self.db.execute("UPDATE message SET preview = ?3 WHERE folder_id = ?1 AND uid = ?2", params![folder_id, uid, preview])?;
        Ok(())
    }

    /// Everyone who has written to the account (not bulk mail, not Junk): (address, the name
    /// they used last, how many messages, the newest date).
    pub fn senders(&self, account: &str) -> rusqlite::Result<Vec<(String, Option<String>, i64, Option<String>)>> {
        let mut st = self.db.prepare(
            "SELECT LOWER(m.from_addr), MAX(m.from_name), COUNT(*), MAX(COALESCE(m.date, m.internal_date))
             FROM message m JOIN folder f ON f.id = m.folder_id
             WHERE f.account = ?1 AND m.from_addr IS NOT NULL AND m.from_addr != '' AND m.bulk = 0
               AND COALESCE(f.special, '') != '\\Junk'
             GROUP BY LOWER(m.from_addr)",
        )?;
        st.query_map([account], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect()
    }

    /// Per folder: messages, their size on the server, and what is downloaded here (bytes).
    pub fn folder_usage(&self, account: &str) -> rusqlite::Result<Vec<(String, i64, i64, i64)>> {
        let mut st = self.db.prepare(
            "SELECT f.name, COUNT(m.id), COALESCE(SUM(m.size), 0), COALESCE(SUM(m.body_size), 0)
             FROM folder f LEFT JOIN message m ON m.folder_id = f.id WHERE f.account = ?1 GROUP BY f.id ORDER BY f.name",
        )?;
        st.query_map([account], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect()
    }

    /// Forget the downloaded copies of an account's messages (they come back when opened).
    pub fn forget_bodies(&self, account: &str) -> rusqlite::Result<usize> {
        self.db.execute(
            "DELETE FROM body WHERE id IN (SELECT m.id FROM message m JOIN folder f ON f.id = m.folder_id WHERE f.account = ?1)",
            [account],
        )?;
        self.db.execute(
            "UPDATE message SET body_size = NULL WHERE body_size IS NOT NULL AND folder_id IN (SELECT id FROM folder WHERE account = ?1)",
            [account],
        )
    }

    /// Every cached message in the folder read.
    pub fn mark_all_read(&self, folder_id: i64) -> rusqlite::Result<()> {
        self.db.execute(
            "UPDATE message SET flags = TRIM(flags || ' \\Seen') WHERE folder_id = ?1 AND ' ' || flags || ' ' NOT LIKE '% \\Seen %'",
            [folder_id],
        )?;
        Ok(())
    }

    /// Forget every cached message in the folder.
    pub fn clear_folder(&self, folder_id: i64) -> rusqlite::Result<()> {
        self.db.execute("DELETE FROM message WHERE folder_id = ?1", [folder_id])?;
        Ok(())
    }

    /// A folder (and its subfolders) renamed on the server: keep the cached mail under the new name.
    pub fn rename_folder(&self, account: &str, from: &str, to: &str, delimiter: Option<&str>) -> rusqlite::Result<()> {
        self.db.execute("UPDATE folder SET name = ?3 WHERE account = ?1 AND name = ?2", params![account, from, to])?;
        if let Some(d) = delimiter.filter(|d| !d.is_empty()) {
            let old = format!("{from}{d}");
            let kids: Vec<(i64, String)> = self
                .db
                .prepare("SELECT id, name FROM folder WHERE account = ?1 AND substr(name, 1, length(?2)) = ?2")?
                .query_map(params![account, old], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (id, name) in kids {
                let rest = name.get(old.len()..).unwrap_or("");
                self.db.execute("UPDATE folder SET name = ?2 WHERE id = ?1", params![id, format!("{to}{d}{rest}")])?;
            }
        }
        Ok(())
    }

    /// Keep a downloaded message, and put its words in the search index.
    pub fn put_body(&self, folder_id: i64, uid: u32, raw: &[u8]) -> rusqlite::Result<()> {
        let id: Option<i64> =
            self.db.query_row("SELECT id FROM message WHERE folder_id = ?1 AND uid = ?2", params![folder_id, uid], |r| r.get(0)).optional()?;
        if let Some(id) = id {
            self.db.execute("INSERT OR REPLACE INTO body(id, raw) VALUES (?1, ?2)", params![id, raw])?;
            self.db.execute("UPDATE message SET body_size = ?2 WHERE id = ?1", params![id, raw.len() as i64])?;
            crate::fulltext::index(&self.db, id, raw)?;
        }
        Ok(())
    }

    /// Index up to `limit` messages not (fully) in the index yet: downloaded ones by their
    /// words, the rest by their headers. How many it did; 0 when the index is complete.
    pub fn index_backlog(&self, account: &str, limit: usize) -> rusqlite::Result<usize> {
        let mut st = self.db.prepare(
            "SELECT m.id, b.raw FROM message m JOIN folder f ON f.id = m.folder_id JOIN body b ON b.id = m.id
             WHERE f.account = ?1 AND m.body_size IS NOT NULL AND m.indexed < 2 LIMIT ?2",
        )?;
        let todo: Vec<(i64, Vec<u8>)> = st.query_map(params![account, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        for (id, raw) in &todo {
            crate::fulltext::index(&self.db, *id, raw)?;
        }
        let left = limit.saturating_sub(todo.len());
        Ok(todo.len() + if left > 0 { crate::fulltext::index_headers(&self.db, account, left)? } else { 0 })
    }

    /// Messages not downloaded yet, newest first: (folder, UID). `since`: only mail dated
    /// on or after it (the same UTC text form the cache keeps dates in).
    pub fn missing_bodies(&self, account: &str, since: Option<&str>, limit: usize) -> rusqlite::Result<Vec<(String, u32)>> {
        let mut st = self.db.prepare(
            "SELECT f.name, m.uid FROM message m JOIN folder f ON f.id = m.folder_id
             WHERE f.account = ?1 AND m.body_size IS NULL AND (?2 IS NULL OR COALESCE(m.date, m.internal_date) >= ?2)
             ORDER BY COALESCE(m.date, m.internal_date) DESC LIMIT ?3",
        )?;
        st.query_map(params![account, since, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
    }

    /// (messages downloaded, messages in all) for an account, or for one folder of it.
    pub fn downloaded(&self, account: &str, folder: Option<&str>) -> rusqlite::Result<(i64, i64)> {
        self.db.query_row(
            "SELECT COALESCE(SUM(m.body_size IS NOT NULL), 0), COUNT(m.id) FROM message m JOIN folder f ON f.id = m.folder_id
             WHERE f.account = ?1 AND (?2 IS NULL OR f.name = ?2)",
            params![account, folder],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn body(&self, folder_id: i64, uid: u32) -> rusqlite::Result<Option<Vec<u8>>> {
        Ok(self
            .db
            .query_row("SELECT b.raw FROM message m JOIN body b ON b.id = m.id WHERE m.folder_id = ?1 AND m.uid = ?2", params![folder_id, uid], |r| {
                r.get(0)
            })
            .optional()?
            .flatten())
    }

    pub fn folders(&self, account: &str) -> rusqlite::Result<Vec<FolderRow>> {
        let mut st = self.db.prepare(
            "SELECT f.id, f.account, f.name, f.delimiter, f.special, f.uidvalidity,
                    (SELECT COUNT(*) FROM message m WHERE m.folder_id = f.id),
                    (SELECT COUNT(*) FROM message m WHERE m.folder_id = f.id AND ' ' || m.flags || ' ' NOT LIKE '% \\Seen %')
             FROM folder f WHERE f.account = ?1
             ORDER BY CASE f.special WHEN '\\Inbox' THEN 0 WHEN '\\Drafts' THEN 1 WHEN '\\Sent' THEN 2 WHEN '\\Archive' THEN 3
                      WHEN '\\Junk' THEN 4 WHEN '\\Trash' THEN 5 ELSE 6 END, f.name",
        )?;
        st.query_map([account], |r| {
            Ok(FolderRow {
                id: r.get(0)?,
                account: r.get(1)?,
                name: r.get(2)?,
                delimiter: r.get(3)?,
                special: r.get(4)?,
                uidvalidity: r.get(5)?,
                total: r.get(6)?,
                unread: r.get(7)?,
            })
        })?
        .collect()
    }

    pub fn folder_id(&self, account: &str, name: &str) -> rusqlite::Result<Option<i64>> {
        self.db.query_row("SELECT id FROM folder WHERE account = ?1 AND name = ?2", params![account, name], |r| r.get(0)).optional()
    }

    /// Newest first.
    /// The highest cached UID in each of an account's folders: where a sync's new mail starts.
    pub fn max_uids(&self, account: &str) -> rusqlite::Result<std::collections::HashMap<String, u32>> {
        let mut st = self.db.prepare(
            "SELECT f.name, COALESCE(MAX(m.uid), 0) FROM folder f LEFT JOIN message m ON m.folder_id = f.id WHERE f.account = ?1 GROUP BY f.id",
        )?;
        st.query_map([account], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
    }

    /// Unread messages above a UID (new since `max_uids`), newest first.
    pub fn unread_above(&self, folder_id: i64, uid: u32, limit: usize) -> rusqlite::Result<Vec<MessageRow>> {
        let mut st = self.db.prepare(&format!(
            "{SELECT_MESSAGE} WHERE folder_id = ?1 AND uid > ?2 AND ' ' || flags || ' ' NOT LIKE '% \\Seen %'
             ORDER BY COALESCE(date, internal_date) DESC, uid DESC LIMIT ?3"
        ))?;
        st.query_map(params![folder_id, uid, limit as i64], row)?.collect()
    }

    pub fn messages(&self, folder_id: i64, limit: usize) -> rusqlite::Result<Vec<MessageRow>> {
        let mut st =
            self.db.prepare(&format!("{SELECT_MESSAGE} WHERE folder_id = ?1 ORDER BY COALESCE(date, internal_date) DESC, uid DESC LIMIT ?2"))?;
        st.query_map(params![folder_id, limit as i64], row)?.collect()
    }
}

const SELECT_MESSAGE: &str = "SELECT m.id, uid, flags, COALESCE(date, internal_date), from_name, from_addr, to_addrs, subject, size, message_id, in_reply_to, body_size IS NOT NULL, f.name, m.categories,
     m.attachments, m.preview, m.bulk, m.internal_date, m.priority
     FROM message m JOIN folder f ON f.id = m.folder_id";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRow> {
    Ok(MessageRow {
        id: r.get(0)?,
        uid: r.get(1)?,
        flags: r.get(2)?,
        date: r.get(3)?,
        from_name: r.get(4)?,
        from_addr: r.get(5)?,
        to: r.get(6)?,
        subject: r.get(7)?,
        size: r.get(8)?,
        message_id: r.get(9)?,
        in_reply_to: r.get(10)?,
        has_body: r.get(11)?,
        folder: r.get(12)?,
        categories: r.get(13)?,
        attachments: r.get::<_, Option<String>>(14)?.map(|a| a.split('\n').filter(|n| !n.is_empty()).map(str::to_string).collect()),
        preview: r.get(15)?,
        bulk: r.get(16)?,
        received: r.get(17)?,
        priority: r.get(18)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_and_removes() {
        let s = Store::open(":memory:").unwrap();
        let f = s.folder("a", "INBOX", None, None).unwrap();
        for (uid, subj, from) in [(1, "Strap photos", "Mark"), (2, "Invoice 50%", "Shop"), (3, "photos of the shroud", "Mark")] {
            s.put_header(f, &Header { uid, subject: Some(subj.into()), from_name: Some(from.into()), ..Default::default() }).unwrap();
        }
        // Encoded, as real mail is: only the index (which decodes) can find the word.
        s.put_body(
            f,
            2,
            b"Subject: Order\r\nContent-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\nWW91ciBvcmRlciBvZiBmaWxhbWVudA==\r\n",
        )
        .unwrap();
        let uids = |v: Vec<MessageRow>| v.into_iter().map(|m| m.uid).collect::<Vec<_>>();
        assert_eq!(uids(s.search(f, "photos", 50).unwrap()), vec![3, 1]);
        assert_eq!(uids(s.search(f, "mark shroud", 50).unwrap()), vec![3]);
        assert_eq!(uids(s.search(f, "50%", 50).unwrap()), vec![2]);
        assert_eq!(uids(s.search(f, "filament", 50).unwrap()), vec![2]);
        assert_eq!(uids(s.search(f, "_", 50).unwrap()), Vec::<u32>::new());
        assert_eq!(uids(s.by_uids(f, &[1, 3]).unwrap()), vec![3, 1]);
        s.set_category(f, 1, "Bills", true).unwrap();
        s.set_category(f, 1, "Red", true).unwrap();
        s.set_category(f, 1, "Bills", false).unwrap();
        assert_eq!(s.by_uids(f, &[1]).unwrap()[0].categories, "Red");
        assert_eq!(uids(s.search(f, "red", 50).unwrap()), vec![1]);
        let other = s.folder("a", "Archive", None, None).unwrap();
        s.put_header(other, &Header { uid: 1, subject: Some("old photos".into()), ..Default::default() }).unwrap();
        let everywhere = s.search_account("a", "photos", 50).unwrap();
        assert_eq!(everywhere.len(), 3);
        assert!(everywhere.iter().any(|m| m.folder == "Archive" && m.uid == 1));
        assert!(s.search_account("b", "photos", 50).unwrap().is_empty());
        s.remove(f, &[1]).unwrap();
        assert_eq!(uids(s.messages(f, 50).unwrap()), vec![3, 2]);
        let p = s.folder("a", "Projects", Some("/"), None).unwrap();
        s.folder("a", "Projects/Printcam", Some("/"), None).unwrap();
        s.put_header(p, &Header { uid: 7, ..Default::default() }).unwrap();
        s.rename_folder("a", "Projects", "Work", Some("/")).unwrap();
        assert!(s.folder_id("a", "Work/Printcam").unwrap().is_some() && s.folder_id("a", "Projects").unwrap().is_none());
        assert_eq!(s.messages(p, 10).unwrap()[0].folder, "Work");
        s.mark_all_read(p).unwrap();
        assert!(s.messages(p, 10).unwrap()[0].seen());
        s.clear_folder(p).unwrap();
        assert!(s.messages(p, 10).unwrap().is_empty());
        s.put_header(f, &Header { uid: 4, flags: "\\Seen \\Flagged".into(), ..Default::default() }).unwrap();
        s.remove_flag(f, 4, "\\Seen").unwrap();
        assert!(!s.by_uids(f, &[4]).unwrap()[0].seen());
    }

    // Read/unread changes on this PC first: the folder's unread count follows at once.
    #[test]
    fn a_flag_changed_here_changes_the_counts() {
        let s = Store::open(":memory:").unwrap();
        let f = s.folder("a", "INBOX", None, None).unwrap();
        for uid in [1, 2] {
            s.put_header(f, &Header { uid, flags: "\\Flagged".into(), ..Default::default() }).unwrap();
        }
        let unread = |s: &Store| s.folders("a").unwrap()[0].unread;
        assert_eq!(unread(&s), 2);
        s.change_flag(f, 1, "\\Seen", true).unwrap();
        assert_eq!(unread(&s), 1);
        assert_eq!(s.messages(f, 10).unwrap().iter().find(|m| m.uid == 1).unwrap().flags, "\\Flagged \\Seen");
        s.change_flag(f, 1, "\\Seen", true).unwrap();
        assert_eq!(s.messages(f, 10).unwrap().iter().find(|m| m.uid == 1).unwrap().flags, "\\Flagged \\Seen", "on twice is still once");
        s.change_flag(f, 1, "\\Seen", false).unwrap();
        assert_eq!(unread(&s), 2);
        s.change_flag(f, 9, "\\Seen", true).unwrap(); // not cached: nothing to change, no error
    }

    #[test]
    fn categories_follow_a_moved_message() {
        let s = Store::open(":memory:").unwrap();
        let inbox = s.folder("a", "INBOX", None, None).unwrap();
        let bills = s.folder("a", "Bills", None, None).unwrap();
        let theirs = s.folder("b", "INBOX", None, None).unwrap();
        let h = |uid| Header { uid, message_id: Some("<m1@x>".into()), ..Default::default() };
        s.put_header(inbox, &h(5)).unwrap();
        s.set_category(inbox, 5, "Red", true).unwrap();
        // Moved: gone from the Inbox, arrives in Bills with a new UID.
        s.remove(inbox, &[5]).unwrap();
        s.put_header(bills, &h(1)).unwrap();
        assert_eq!(s.messages(bills, 10).unwrap()[0].categories, "Red");
        // Another account's copy of the same message is its own.
        s.put_header(theirs, &h(1)).unwrap();
        assert_eq!(s.messages(theirs, 10).unwrap()[0].categories, "");
        // Taking the last category off forgets it.
        s.set_category(bills, 1, "Red", false).unwrap();
        s.put_header(inbox, &h(6)).unwrap();
        assert_eq!(s.messages(inbox, 10).unwrap()[0].categories, "");
    }

    // Restore from Trash: where a deleted message came from is kept by Message-ID, so it is
    // still known once the message is in Trash under a new UID.
    #[test]
    fn trash_remembers_where_mail_came_from() {
        let s = Store::open(":memory:").unwrap();
        let bills = s.folder("a", "Bills", None, None).unwrap();
        let inbox = s.folder("a", "INBOX", None, Some("\\Inbox")).unwrap();
        let trash = s.folder("a", "Trash", None, Some("\\Trash")).unwrap();
        let theirs = s.folder("b", "Trash", None, Some("\\Trash")).unwrap();
        let h = |uid, id: Option<&str>| Header { uid, message_id: id.map(str::to_string), ..Default::default() };
        s.put_header(bills, &h(5, Some("<m1@x>"))).unwrap();
        s.put_header(bills, &h(6, None)).unwrap();
        s.put_header(inbox, &h(9, Some("<m2@x>"))).unwrap();
        // Deleted: remembered first, then taken out of the cache.
        assert_eq!(s.remember_origin(bills, &[5, 6, 77]).unwrap(), 1, "no Message-ID, or not cached: nothing to remember");
        s.remove(bills, &[5, 6]).unwrap();
        // In Trash with new UIDs (and the one without a Message-ID).
        s.put_header(trash, &h(1, Some("<m1@x>"))).unwrap();
        s.put_header(trash, &h(2, None)).unwrap();
        s.put_header(trash, &h(3, Some("<other@x>"))).unwrap();
        // Another account's copy of the same message knows nothing of this one's.
        s.put_header(theirs, &h(1, Some("<m1@x>"))).unwrap();
        let got = s.origins(trash, &[1, 2, 3, 99]).unwrap();
        assert_eq!(got.len(), 3, "a UID not cached is left out");
        assert_eq!(got[0], Origin { uid: 1, message_id: Some("<m1@x>".into()), folder: Some("Bills".into()) });
        assert_eq!(got[1], Origin { uid: 2, message_id: None, folder: None });
        assert_eq!(got[2].folder, None, "deleted elsewhere: not known");
        assert_eq!(s.origins(theirs, &[1]).unwrap()[0].folder, None);
        // Deleted again from somewhere else: the newer place wins.
        s.put_header(inbox, &h(10, Some("<m1@x>"))).unwrap();
        s.remember_origin(inbox, &[10]).unwrap();
        assert_eq!(s.origins(trash, &[1]).unwrap()[0].folder.as_deref(), Some("INBOX"));
        // Restored: forgotten.
        assert_eq!(s.forget_origins("a", &["<m1@x>".into(), "<never@x>".into()]).unwrap(), 1);
        assert_eq!(s.origins(trash, &[1]).unwrap()[0].folder, None);
        // A whole folder emptied into Trash; then Trash deleted for good, one and then all.
        s.remember_folder_origin(inbox).unwrap();
        s.put_header(trash, &h(4, Some("<m2@x>"))).unwrap();
        assert_eq!(s.origins(trash, &[4]).unwrap()[0].folder.as_deref(), Some("INBOX"));
        s.put_header(trash, &h(11, Some("<m1@x>"))).unwrap();
        s.remember_origin(inbox, &[10]).unwrap();
        assert_eq!(s.forget_origins_in(trash, Some(&[11])).unwrap(), 1);
        assert_eq!(s.origins(trash, &[4]).unwrap()[0].folder.as_deref(), Some("INBOX"), "only the one deleted for good");
        assert_eq!(s.forget_origins_in(trash, None).unwrap(), 1);
        assert_eq!(s.origins(trash, &[4]).unwrap()[0].folder, None);
    }

    // Trash emptied by another app: once a full sync no longer finds the message anywhere, its
    // record goes, but not while a message just moved may still be on its way into Trash here.
    #[test]
    fn records_of_mail_gone_elsewhere_are_pruned() {
        let s = Store::open(":memory:").unwrap();
        let inbox = s.folder("a", "INBOX", None, None).unwrap();
        let trash = s.folder("a", "Trash", None, None).unwrap();
        let h = |uid, id: &str| Header { uid, message_id: Some(id.into()), ..Default::default() };
        s.put_header(inbox, &h(1, "<gone@x>")).unwrap();
        s.put_header(inbox, &h(2, "<kept@x>")).unwrap();
        s.remember_origin(inbox, &[1, 2]).unwrap();
        s.remove(inbox, &[1, 2]).unwrap();
        s.put_header(trash, &h(7, "<kept@x>")).unwrap();
        assert_eq!(s.prune_origins("a", 86_400).unwrap(), 0, "too young to go");
        assert_eq!(s.prune_origins("b", -60).unwrap(), 0, "another account's");
        assert_eq!(s.prune_origins("a", -60).unwrap(), 1, "old enough, and nowhere in the account");
        assert_eq!(s.origins(trash, &[7]).unwrap()[0].folder.as_deref(), Some("INBOX"));
    }

    #[test]
    fn caches_folders_headers_and_bodies() {
        let s = Store::open(":memory:").unwrap();
        let inbox = s.folder("me@example.com", "INBOX", Some("/"), Some("\\Inbox")).unwrap();
        let sent = s.folder("me@example.com", "Sent", Some("/"), Some("\\Sent")).unwrap();
        assert_eq!(s.folder("me@example.com", "INBOX", Some("/"), Some("\\Inbox")).unwrap(), inbox);
        s.reset_folder(inbox, 7).unwrap();
        for uid in 1..=3 {
            s.put_header(
                inbox,
                &Header {
                    uid,
                    flags: if uid == 1 { "\\Seen".into() } else { String::new() },
                    subject: Some(format!("m{uid}")),
                    date: Some(format!("2026-10-0{uid}T10:00:00Z")),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        assert_eq!(s.sync_state(inbox).unwrap(), (Some(7), 3));
        let f = s.folders("me@example.com").unwrap();
        assert_eq!((f[0].name.as_str(), f[0].total, f[0].unread), ("INBOX", 3, 2));
        assert_eq!(f[1].id, sent);
        let m = s.messages(inbox, 10).unwrap();
        assert_eq!(m.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![3, 2, 1]);
        assert_eq!(s.keep_only(inbox, &[1, 3]).unwrap(), 1);
        s.put_body(inbox, 3, b"raw mail").unwrap();
        assert_eq!(s.body(inbox, 3).unwrap().as_deref(), Some(&b"raw mail"[..]));
        s.put_header(inbox, &Header { uid: 10, from_addr: Some("Ann@Example.org".into()), from_name: Some("Ann".into()), ..Default::default() })
            .unwrap();
        s.put_header(inbox, &Header { uid: 11, from_addr: Some("ann@example.org".into()), ..Default::default() }).unwrap();
        s.put_header(inbox, &Header { uid: 12, from_addr: Some("news@shop.example".into()), bulk: true, ..Default::default() }).unwrap();
        let who = s.senders("me@example.com").unwrap();
        assert_eq!(who.len(), 1);
        assert_eq!((who[0].0.as_str(), who[0].1.as_deref(), who[0].2), ("ann@example.org", Some("Ann"), 2));
        s.remove(inbox, &[10, 11, 12]).unwrap();
        let usage = s.folder_usage("me@example.com").unwrap();
        assert_eq!(usage.iter().find(|u| u.0 == "INBOX").map(|u| (u.1, u.3)), Some((2, 8)));
        assert_eq!(usage.iter().find(|u| u.0 == "Sent").map(|u| u.1), Some(0));
        assert_eq!(s.forget_bodies("me@example.com").unwrap(), 1);
        assert_eq!(s.body(inbox, 3).unwrap(), None);
        s.reset_folder(inbox, 8).unwrap();
        assert_eq!(s.sync_state(inbox).unwrap(), (Some(8), 0));
    }

    #[test]
    fn offline_download_and_index() {
        let s = Store::open(":memory:").unwrap();
        let f = s.folder("a@b", "INBOX", None, None).unwrap();
        for (uid, date) in [(1, "2025-01-01T00:00:00Z"), (2, "2026-10-01T00:00:00Z"), (3, "2026-10-09T00:00:00Z")] {
            s.put_header(f, &Header { uid, date: Some(date.into()), ..Default::default() }).unwrap();
        }
        assert_eq!(s.missing_bodies("a@b", None, 10).unwrap(), vec![("INBOX".to_string(), 3), ("INBOX".into(), 2), ("INBOX".into(), 1)]);
        assert_eq!(s.missing_bodies("a@b", Some("2026-01-01T00:00:00Z"), 10).unwrap().len(), 2);
        s.put_body(f, 3, b"Subject: s\r\n\r\nthe shroud fits").unwrap();
        assert_eq!(s.downloaded("a@b", None).unwrap(), (1, 3));
        assert_eq!(s.downloaded("a@b", Some("INBOX")).unwrap(), (1, 3));
        // A body cached before the index existed is indexed by the backlog pass.
        let id: i64 = s.db.query_row("SELECT id FROM message WHERE uid = 1", [], |r| r.get(0)).unwrap();
        s.db.execute("INSERT INTO body(id, raw) VALUES (?1, ?2)", params![id, b"Subject: s\r\n\r\nold strap photos".as_slice()]).unwrap();
        s.db.execute("UPDATE message SET body_size = 25 WHERE id = ?1", [id]).unwrap();
        assert_eq!(s.index_backlog("a@b", 10).unwrap(), 1);
        assert_eq!(s.index_backlog("a@b", 10).unwrap(), 0);
        let uids = |v: Vec<MessageRow>| v.into_iter().map(|m| m.uid).collect::<Vec<_>>();
        assert_eq!(uids(s.search_account("a@b", "strap", 10).unwrap()), vec![1]);
        assert_eq!(uids(s.search_account("a@b", "shro", 10).unwrap()), vec![3]);
        // Words with FTS syntax in them are just words.
        assert!(s.search_account("a@b", "\"OR NEAR( *", 10).is_ok());
        // Gone from the server, gone from the index.
        s.remove(f, &[1, 2]).unwrap();
        let left: i64 = s.db.query_row("SELECT COUNT(*) FROM message_fts", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 1);
        let bodies: i64 = s.db.query_row("SELECT COUNT(*) FROM body", [], |r| r.get(0)).unwrap();
        assert_eq!(bodies, 1);
        s.forget_bodies("a@b").unwrap();
        assert_eq!(s.downloaded("a@b", None).unwrap(), (0, 1));
        // Still found by its words after the copy is cleared: the index stays.
        assert_eq!(uids(s.search_account("a@b", "shroud", 10).unwrap()), vec![3]);
        // A category set on another PC arrives as a keyword in the flags at the next sync.
        s.set_flags(f, 3, "\\Seen Projects").unwrap();
        assert_eq!(uids(s.search_account("a@b", "projects", 10).unwrap()), vec![3]);
        s.set_flags(f, 3, "\\Seen").unwrap();
        assert!(s.search_account("a@b", "projects", 10).unwrap().is_empty());
        assert!(s.search_account("a@b", "seen", 10).unwrap().is_empty());
        // Categories: found by them as soon as they are set, and not after.
        s.set_category(f, 3, "Receipts", true).unwrap();
        assert_eq!(uids(s.search_account("a@b", "receipts", 10).unwrap()), vec![3]);
        s.set_category(f, 3, "Receipts", false).unwrap();
        assert!(s.search_account("a@b", "receipts", 10).unwrap().is_empty());
        // A message that has just synced is found by its sender before it is downloaded.
        s.put_header(f, &Header { uid: 9, from_name: Some("Bambu Lab".into()), subject: Some("Shipped".into()), ..Default::default() }).unwrap();
        assert_eq!(uids(s.search_account("a@b", "bambu", 10).unwrap()), vec![9]);
    }

    #[test]
    fn mail_deleted_elsewhere_leaves_body_and_index() {
        let s = Store::open(":memory:").unwrap();
        let f = s.folder("a@b", "INBOX", None, None).unwrap();
        let t = s.folder("a@b", "Trash", None, None).unwrap();
        for uid in 1..=3 {
            s.put_header(f, &Header { uid, subject: Some(format!("strap {uid}")), ..Default::default() }).unwrap();
            s.put_body(f, uid, format!("Subject: strap {uid}\r\n\r\nzebra words").as_bytes()).unwrap();
        }
        let count = |sql: &str| s.db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!((count("SELECT COUNT(*) FROM body"), count("SELECT COUNT(*) FROM message_fts")), (3, 3));
        // The other PC deleted 2 (into Trash, where it has a new UID): this sync's view.
        assert_eq!(s.keep_only(f, &[1, 3]).unwrap(), 1);
        s.put_header(t, &Header { uid: 50, subject: Some("strap 2".into()), ..Default::default() }).unwrap();
        assert_eq!((count("SELECT COUNT(*) FROM body"), count("SELECT COUNT(*) FROM message_fts")), (2, 3));
        let found: Vec<(String, u32)> = s.search_account("a@b", "zebra", 10).unwrap().into_iter().map(|m| (m.folder, m.uid)).collect();
        assert_eq!(found, vec![("INBOX".to_string(), 3), ("INBOX".into(), 1)]);
        let found: Vec<(String, u32)> = s.search_account("a@b", "strap", 10).unwrap().into_iter().map(|m| (m.folder, m.uid)).collect();
        assert!(found.contains(&("Trash".into(), 50)) && !found.contains(&("INBOX".into(), 2)));
        // A folder deleted elsewhere takes its mail, bodies and index entries with it.
        s.prune_folders("a@b", &["Trash".into()]).unwrap();
        assert_eq!((count("SELECT COUNT(*) FROM message"), count("SELECT COUNT(*) FROM body"), count("SELECT COUNT(*) FROM message_fts")), (1, 0, 1));
    }

    #[test]
    fn bodies_move_out_of_old_caches() {
        let dir = std::env::temp_dir().join(format!("mc-migrate-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("mail.db");
        let _ = std::fs::remove_file(&path);
        {
            // A cache as versions before the body table left it.
            let db = Connection::open(&path).unwrap();
            db.execute_batch(
                "CREATE TABLE folder (id INTEGER PRIMARY KEY, account TEXT NOT NULL, name TEXT NOT NULL, delimiter TEXT,
                   special TEXT, uidvalidity INTEGER, uidnext INTEGER, UNIQUE(account, name));
                 CREATE TABLE message (id INTEGER PRIMARY KEY, folder_id INTEGER NOT NULL REFERENCES folder(id) ON DELETE CASCADE,
                   uid INTEGER NOT NULL, flags TEXT NOT NULL DEFAULT '', internal_date TEXT, size INTEGER,
                   date TEXT, from_name TEXT, from_addr TEXT, to_addrs TEXT, subject TEXT,
                   message_id TEXT, in_reply_to TEXT, raw BLOB, UNIQUE(folder_id, uid));
                 INSERT INTO folder(id, account, name) VALUES (1, 'a@b', 'INBOX');
                 INSERT INTO message(folder_id, uid, raw) VALUES (1, 7, CAST('Subject: x' || char(13,10,13,10) || 'kept strap' AS BLOB)), (1, 8, NULL);",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.downloaded("a@b", None).unwrap(), (1, 2));
        assert!(s.body(1, 7).unwrap().is_some_and(|b| b.ends_with(b"kept strap")));
        let left: i64 = s.db.query_row("SELECT COUNT(*) FROM message WHERE raw IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        // The downloaded one by its words, the other by its headers.
        assert_eq!(s.index_backlog("a@b", 10).unwrap(), 2);
        assert_eq!(s.index_backlog("a@b", 10).unwrap(), 0);
        assert_eq!(s.search_account("a@b", "strap", 10).unwrap().len(), 1);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn follows_messages_by_message_id() {
        let s = Store::open(":memory:").unwrap();
        let inbox = s.folder("a", "INBOX", None, None).unwrap();
        let trash = s.folder("a", "Trash", None, None).unwrap();
        for (uid, id) in [(1, Some("<one@x>")), (2, None), (3, Some("<three@x>"))] {
            s.put_header(inbox, &Header { uid, message_id: id.map(String::from), ..Default::default() }).unwrap();
        }
        // No Message-ID: left out (it cannot be followed).
        assert_eq!(s.message_ids(inbox, &[1, 2, 3, 9]).unwrap(), vec!["<one@x>".to_string(), "<three@x>".to_string()]);
        // In Trash twice (deleted, restored, deleted again): the newest copy.
        for (uid, id) in [(4, "<one@x>"), (7, "<one@x>")] {
            s.put_header(trash, &Header { uid, message_id: Some(id.into()), ..Default::default() }).unwrap();
        }
        let ids = vec!["<one@x>".to_string(), "<three@x>".to_string()];
        assert_eq!(s.find_message_ids(trash, &ids).unwrap(), vec![("<one@x>".to_string(), 7)]);
        assert!(s.find_message_ids(inbox, &["<nope@x>".to_string()]).unwrap().is_empty());
        // Both are in the Inbox as well as (one of them) in Trash; one nowhere else is not.
        assert_eq!(s.message_ids_elsewhere("a", trash, &ids).unwrap(), 2);
        assert_eq!(s.message_ids_elsewhere("a", inbox, &ids).unwrap(), 1);
        assert_eq!(s.message_ids_elsewhere("b", trash, &ids).unwrap(), 0);
    }
}
