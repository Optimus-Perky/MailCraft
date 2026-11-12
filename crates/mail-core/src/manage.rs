//! Tidying mail on the server: moving, deleting, and searching a folder.

use crate::Error;
use crate::connect::Session;
use crate::store::{FolderRow, Store};
use futures::TryStreamExt;

fn uid_set(uids: &[u32]) -> String {
    uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

/// The account's Trash: the folder marked \Trash, else one with a usual name, else "Trash" (created).
pub async fn trash_folder(session: &mut Session, store: &Store, account: &str) -> Result<String, Error> {
    if let Some(t) = find_trash(&store.folders(account)?) {
        return Ok(t);
    }
    let _ = session.create("Trash").await;
    Ok("Trash".into())
}

/// Which of an account's folders is its Trash, without asking the server: the one marked
/// \Trash, else one with a usual name. The app and the worker must agree on it.
pub fn find_trash(folders: &[FolderRow]) -> Option<String> {
    if let Some(f) = folders.iter().find(|f| f.special.as_deref() == Some("\\Trash")) {
        return Some(f.name.clone());
    }
    ["Trash", "Deleted Items", "Deleted Messages", "Bin", "[Gmail]/Trash", "[Gmail]/Bin", "INBOX.Trash", "INBOX/Trash"]
        .iter()
        .find_map(|guess| folders.iter().find(|f| f.name.eq_ignore_ascii_case(guess)))
        .map(|f| f.name.clone())
}

/// Move messages to another folder: UID MOVE where the server has it, else copy, flag
/// \Deleted and expunge.
pub async fn move_messages(session: &mut Session, from: &str, uids: &[u32], to: &str) -> Result<(), Error> {
    if uids.is_empty() {
        return Ok(());
    }
    let can_move = session.capabilities().await?.has_str("MOVE");
    session.select(from).await?;
    let set = uid_set(uids);
    if can_move {
        session.uid_mv(&set, to).await?;
        return Ok(());
    }
    session.uid_copy(&set, to).await?;
    expunge(session, uids).await
}

/// Flag \Deleted and expunge in the selected folder (UID EXPUNGE where offered, else EXPUNGE).
pub(crate) async fn expunge(session: &mut Session, uids: &[u32]) -> Result<(), Error> {
    if uids.is_empty() {
        return Ok(());
    }
    let set = uid_set(uids);
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

/// What a delete did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deleted {
    MovedTo(String),
    Gone,
}

/// Delete as mail apps do: into Trash, or for good when they are already in Trash.
pub async fn delete_messages(session: &mut Session, store: &Store, account: &str, folder: &str, uids: &[u32]) -> Result<Deleted, Error> {
    let trash = trash_folder(session, store, account).await?;
    if folder == trash {
        session.select(folder).await?;
        expunge(session, uids).await?;
        return Ok(Deleted::Gone);
    }
    // Where they came from, for Restore (the app has usually noted it already, before taking
    // them off the screen; rules deleting new mail have not).
    if let Some(id) = store.folder_id(account, folder)? {
        store.remember_origin(id, uids)?;
    }
    move_messages(session, folder, uids, &trash).await?;
    Ok(Deleted::MovedTo(trash))
}

/// Search the whole text of the messages in a folder on the server (IMAP SEARCH TEXT).
/// Returns their UIDs. Plain-ASCII words only: other text is searched in the local cache.
/// The account's storage in use and its limit, in KB, when the server says (RFC 2087 QUOTA).
pub async fn quota(session: &mut Session) -> Result<Option<(u64, u64)>, Error> {
    use async_imap::types::QuotaResourceName;
    let (_, quotas) = session.get_quota_root("INBOX").await?;
    Ok(quotas.iter().flat_map(|q| q.resources.iter()).find(|r| matches!(r.name, QuotaResourceName::Storage)).map(|r| (r.usage, r.limit)))
}

pub async fn search(session: &mut Session, folder: &str, text: &str) -> Result<Vec<u32>, Error> {
    let words: Vec<&str> = text.split_whitespace().filter(|w| w.is_ascii()).collect();
    if words.is_empty() {
        return Ok(vec![]);
    }
    session.select(folder).await?;
    let q = words.iter().map(|w| format!("TEXT \"{}\"", w.replace('\\', "\\\\").replace('"', "\\\""))).collect::<Vec<_>>().join(" ");
    let mut uids: Vec<u32> = session.uid_search(q).await?.into_iter().collect();
    uids.sort_unstable();
    Ok(uids)
}

/// Search several folders in one session; a folder that cannot be searched is skipped.
pub async fn search_folders(session: &mut Session, folders: &[String], text: &str) -> Vec<(String, Vec<u32>)> {
    let mut out = vec![];
    for f in folders {
        if let Ok(uids) = search(session, f, text).await
            && !uids.is_empty()
        {
            out.push((f.clone(), uids));
        }
    }
    out
}

/// Folders mail apps keep for a purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Special {
    Inbox,
    Archive,
    Junk,
}

/// The account's folder for a purpose: the one marked for it, else one with a usual name, else
/// a new one (Inbox always exists).
pub async fn special_folder(session: &mut Session, store: &Store, account: &str, which: Special) -> Result<String, Error> {
    let folders = store.folders(account)?;
    let (mark, names, make): (&str, &[&str], &str) = match which {
        Special::Inbox => ("\\Inbox", &["INBOX"], "INBOX"),
        Special::Archive => ("\\Archive", &["Archive", "Archives", "INBOX.Archive", "INBOX/Archive"], "Archive"),
        Special::Junk => ("\\Junk", &["Junk", "Spam", "Junk E-mail", "Junk Email", "Bulk Mail", "[Gmail]/Spam", "INBOX.Junk", "INBOX/Junk"], "Junk"),
    };
    if let Some(f) = folders.iter().find(|f| f.special.as_deref() == Some(mark)) {
        return Ok(f.name.clone());
    }
    for guess in names {
        if let Some(f) = folders.iter().find(|f| f.name.eq_ignore_ascii_case(guess)) {
            return Ok(f.name.clone());
        }
    }
    if which != Special::Inbox {
        let _ = session.create(make).await;
    }
    Ok(make.into())
}

/// Copy messages to another folder (the originals stay).
pub async fn copy_messages(session: &mut Session, from: &str, uids: &[u32], to: &str) -> Result<(), Error> {
    if uids.is_empty() {
        return Ok(());
    }
    session.select(from).await?;
    session.uid_copy(uid_set(uids), to).await?;
    Ok(())
}

/// Add or take off a flag on several messages.
pub async fn set_flag(session: &mut Session, folder: &str, uids: &[u32], flag: &str, on: bool) -> Result<(), Error> {
    if uids.is_empty() {
        return Ok(());
    }
    session.select(folder).await?;
    let op = if on { "+FLAGS" } else { "-FLAGS" };
    let _: Vec<_> = session.uid_store(uid_set(uids), format!("{op} ({flag})")).await?.try_collect().await?;
    Ok(())
}

/// Every message in the folder read.
pub async fn mark_all_read(session: &mut Session, folder: &str) -> Result<(), Error> {
    let mb = session.select(folder).await?;
    if mb.exists > 0 {
        let _: Vec<_> = session.store("1:*", "+FLAGS (\\Seen)").await?.try_collect().await?;
    }
    Ok(())
}

/// Delete everything in the folder for good (for Trash and Junk).
pub async fn empty_folder(session: &mut Session, folder: &str) -> Result<usize, Error> {
    let mb = session.select(folder).await?;
    if mb.exists == 0 {
        return Ok(0);
    }
    let _: Vec<_> = session.store("1:*", "+FLAGS (\\Deleted)").await?.try_collect().await?;
    let _: Vec<_> = session.expunge().await?.try_collect().await?;
    Ok(mb.exists as usize)
}

/// Move everything in a folder to another (emptying an ordinary folder into Trash).
pub async fn move_all(session: &mut Session, from: &str, to: &str) -> Result<usize, Error> {
    let mb = session.select(from).await?;
    if mb.exists == 0 {
        return Ok(0);
    }
    let uids: Vec<u32> = session.uid_search("ALL").await?.into_iter().collect();
    move_messages(session, from, &uids, to).await?;
    Ok(uids.len())
}

pub async fn create_folder(session: &mut Session, name: &str) -> Result<(), Error> {
    session.create(name).await?;
    let _ = session.subscribe(name).await;
    Ok(())
}

pub async fn rename_folder(session: &mut Session, from: &str, to: &str) -> Result<(), Error> {
    session.rename(from, to).await?;
    let _ = session.subscribe(to).await;
    Ok(())
}

pub async fn delete_folder(session: &mut Session, name: &str) -> Result<(), Error> {
    let _ = session.unsubscribe(name).await;
    session.delete(name).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn uid_sets() {
        assert_eq!(super::uid_set(&[3, 9, 12]), "3,9,12");
    }

    #[test]
    fn trash_is_the_marked_folder_else_a_usual_name() {
        let f = |name: &str, special: Option<&str>| crate::FolderRow {
            id: 0,
            account: "a".into(),
            name: name.into(),
            delimiter: Some("/".into()),
            special: special.map(str::to_string),
            uidvalidity: None,
            total: 0,
            unread: 0,
        };
        assert_eq!(super::find_trash(&[f("INBOX", None), f("Bin", None), f("Rubbish", Some("\\Trash"))]).as_deref(), Some("Rubbish"));
        assert_eq!(super::find_trash(&[f("INBOX", None), f("[Gmail]/Bin", None)]).as_deref(), Some("[Gmail]/Bin"));
        assert_eq!(super::find_trash(&[f("INBOX", None), f("deleted items", None)]).as_deref(), Some("deleted items"));
        assert_eq!(super::find_trash(&[f("INBOX", None), f("Projects/Trash", None)]), None, "a subfolder named so is not it");
        assert_eq!(super::find_trash(&[]), None);
    }
}
