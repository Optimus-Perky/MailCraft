//! Network work off the UI thread: finding settings, syncing, fetching messages.

use crate::accounts::Account;
use mailcraft_core::Store;
use mailcraft_core::oauth::{self, Provider};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub enum Request {
    Discover(String),
    Sync(Account, String),
    Open(Account, String, String, u32),
    /// Send, keep a copy in Sent, flag the message replied to (folder, uid) as answered,
    /// and remove the saved draft with this Message-ID, if there is one.
    /// Last: send through another server than the account's (a send-only address).
    Send(Account, String, Box<mailcraft_core::send::Outgoing>, Option<(String, u32)>, Option<String>, Option<Box<Via>>),
    /// Sign in to a send-only address's server, sending nothing: (address, how).
    TestSmtp(String, Box<Via>),
    /// Sign in to an account's incoming server and out again (Settings > Edit).
    TestImap(Account, String),
    /// Sign in to an account's outgoing server, sending nothing.
    TestSmtpAccount(Account, String),
    /// Send without keeping a copy (read receipts).
    SendOnly(Account, String, Box<mailcraft_core::send::Outgoing>),
    /// Save to Drafts, replacing the older copy with the same Message-ID.
    SaveDraft(Account, String, Box<mailcraft_core::send::Outgoing>),
    /// Remove a saved draft by its Message-ID.
    DeleteDraft(Account, String, String),
    /// Sign in through the provider's page in the browser (OAuth accounts).
    SignIn(Account),
    /// A calendar account's browser sign-in (Google).
    CalSignIn(crate::calendar::CalSource),
    /// Stop the server search that is running (its connection is dropped, so the server stops too).
    CancelSearch,
    /// Download every message the cache lacks (dated on or after the UTC text given, if one
    /// is), newest first, so it can be read and searched on this PC.
    Download(Account, String, Option<String>),
    /// Stop downloading (Settings > Storage: only when opened).
    StopDownload,
    /// Delete (into Trash, or for good from Trash): folder, UIDs.
    Delete(Account, String, String, Vec<u32>),
    /// Move: from folder, UIDs, to folder.
    Move(Account, String, String, Vec<u32>, String),
    /// Put messages in Trash back where they were deleted from.
    Restore(Account, String, Restore),
    /// Bring one folder up to date with the server (F5), leaving the others to the next sync.
    RefreshFolder(Account, String, String),
    /// Give messages a category (an IMAP keyword) or take it off: (folder, UIDs) groups, keyword, on.
    Categorise(Account, String, Vec<(String, Vec<u32>)>, String, bool),
    /// Run the account's rules over every message in a folder now.
    RunRules(Account, String, String),
    /// Folder housekeeping.
    Folder(Account, String, FolderOp),
    /// Move to the account's Archive, Junk or Inbox: from folder, UIDs.
    MoveSpecial(Account, String, String, Vec<u32>, mailcraft_core::manage::Special),
    /// Copy: from folder, UIDs, to folder.
    Copy(Account, String, String, Vec<u32>, String),
    /// Flag on or off: folder, UIDs, flag, on.
    SetFlag(Account, String, String, Vec<u32>, String, bool),
    /// Search full text on the server: one folder, or None for every folder; words.
    Search(Account, String, Option<String>, String),
    /// Add a flag on the server and in the cache: (folder, uid, flag).
    Flag(Account, String, String, u32, String),
    /// How much of the account's storage is in use.
    Quota(Account, String),
    /// Calendar work (calendar.rs).
    Cal(crate::calendar::CalRequest),
}

#[derive(Debug, Clone)]
pub enum FolderOp {
    Create(String),
    /// (old name, new name)
    Rename(String, String),
    Delete(String),
    MarkAllRead(String),
    Empty(String),
    /// The account's Trash, whatever it is called.
    EmptyTrash,
    /// Everything in an ordinary folder moved to Trash (Outlook's Empty folder).
    EmptyToTrash(String),
}

/// Messages going back from Trash to one folder.
#[derive(Debug, Clone)]
pub struct Restore {
    pub trash: String,
    pub uids: Vec<u32>,
    pub to: String,
    /// `to` as the folder pane shows it ("Inbox" for INBOX).
    pub label: String,
    /// Their Message-IDs: where they came from is forgotten once they are back.
    pub message_ids: Vec<String>,
    /// How many of them go to `to` (the Inbox) only because where they came from is not known.
    pub unknown: usize,
}

/// A send-only address's server and sign-in.
pub struct Via {
    pub endpoint: mailcraft_core::Endpoint,
    pub username: String,
    pub password: String,
    /// Signs in through the provider (Outlook.com, Gmail): the address as an account, whose
    /// saved sign-in gives the token. Microsoft refuses passwords for its addresses.
    pub oauth: Option<Account>,
}

/// Sign in to a send-only address's server, as sending would.
async fn via_login(v: &Via) -> Result<Credential, String> {
    match &v.oauth {
        Some(a) => credential(a, "").await,
        None => Ok(Credential::Password(v.password.clone())),
    }
}

pub enum Event {
    /// A send-only address's server answered the Test button: (address, result).
    SmtpTested(String, Result<(), String>),
    /// An account's incoming server answered the Test button.
    ImapTested(String, Result<(), String>),
    /// An account's outgoing server answered Test sending.
    AccountSmtpTested(String, Result<(), String>),
    /// Categories changed; a note when the server would not keep them.
    Categorised(Option<String>),
    /// Something finished that only needs reporting (and a sync to show it).
    Done(Result<String, String>),
    Discovered(String, Result<mailcraft_autoconfig::Settings, String>),
    /// (address, the sync report and what the rules did to new mail, if anything)
    Synced(String, Result<(mailcraft_core::sync::Report, Option<String>), String>),
    Opened(String, String, u32, Result<Vec<u8>, String>),
    Sent(Result<String, String>),
    /// (draft Message-ID, the folder it went in)
    DraftSaved(String, Result<String, String>),
    DraftDeleted(Result<(), String>),
    /// (address, the address the provider says was signed in, if it said)
    SignedIn(String, Result<Option<String>, String>),
    /// A server search across folders: (folders done, folders in all).
    SearchProgress(usize, usize),
    /// Downloading for offline reading and search: (account, messages here, messages in all).
    Downloading(String, i64, i64),
    /// The download stopped: done, or why not.
    Downloaded(String, Result<(), String>),
    /// A move or delete finished: what to say about it.
    Moved(Result<String, String>),
    /// One folder refreshed (F5): (address, folder, the report and what the rules did, if anything).
    FolderRefreshed(String, String, Result<(mailcraft_core::sync::Report, Option<String>), String>),
    /// (folder or None for all, words, matching UIDs per folder)
    Searched(Option<String>, String, Result<Vec<(String, Vec<u32>)>, String>),
    Flagged(Result<(), String>),
    /// (address, KB in use and the limit, if the server keeps a quota)
    Quota(String, Result<Option<(u64, u64)>, String>),
    Cal(crate::calendar::CalEvent),
}

impl Request {
    /// What the request is, for the log: its kind, account and folder, never what it carries
    /// to sign in with.
    fn describe(&self) -> String {
        use crate::calendar::CalRequest as C;
        match self {
            Request::Discover(e) => format!("find settings for {e}"),
            Request::Sync(a, _) => format!("sync {}", a.email),
            Request::Open(a, _, f, uid) => format!("open {} {f} #{uid}", a.email),
            Request::Send(a, ..) => format!("send from {}", a.email),
            Request::TestSmtp(e, _) => format!("test sending as {e}"),
            Request::TestImap(a, _) => format!("test {} incoming", a.email),
            Request::TestSmtpAccount(a, _) => format!("test {} outgoing", a.email),
            Request::SendOnly(a, ..) => format!("send a receipt from {}", a.email),
            Request::SaveDraft(a, ..) => format!("save a draft in {}", a.email),
            Request::DeleteDraft(a, ..) => format!("delete a draft in {}", a.email),
            Request::SignIn(a) => format!("sign in {}", a.email),
            Request::CalSignIn(c) => format!("sign in calendar {}", c.label),
            Request::CancelSearch => "cancel the search".into(),
            Request::Download(a, ..) => format!("download {} for offline", a.email),
            Request::StopDownload => "stop downloading".into(),
            Request::Delete(a, _, f, u) => format!("delete {} from {} {f}", count(u.len()), a.email),
            Request::Move(a, _, f, u, t) => format!("move {} in {} from {f} to {t}", count(u.len()), a.email),
            Request::Restore(a, _, r) => {
                format!("restore {} in {} from {} to {} ({} not known)", count(r.uids.len()), a.email, r.trash, r.to, r.unknown)
            }
            Request::RefreshFolder(a, _, f) => format!("refresh {} {f}", a.email),
            Request::Categorise(a, ..) => format!("categorise in {}", a.email),
            Request::RunRules(a, _, f) => format!("run rules on {} {f}", a.email),
            Request::Folder(a, _, op) => format!("folder {op:?} in {}", a.email),
            Request::MoveSpecial(a, _, f, u, w) => format!("move {} in {} from {f} to {w:?}", count(u.len()), a.email),
            Request::Copy(a, _, f, u, t) => format!("copy {} in {} from {f} to {t}", count(u.len()), a.email),
            Request::SetFlag(a, _, f, u, flag, on) => {
                format!("{} {flag} on {} in {} {f}", if *on { "set" } else { "clear" }, count(u.len()), a.email)
            }
            Request::Search(a, _, f, _) => format!("search {} {}", a.email, f.as_deref().unwrap_or("every folder")),
            Request::Flag(a, _, f, uid, flag) => format!("set {flag} on {} {f} #{uid}", a.email),
            Request::Quota(a, _) => format!("quota of {}", a.email),
            Request::Cal(C::Test(c, _)) => format!("test calendar {}", c.label),
            Request::Cal(C::Sync(c, _)) => format!("sync calendar {}", c.label),
            Request::Cal(C::Save(c, ..)) => format!("save an event in {}", c.label),
            Request::Cal(C::Delete(c, ..)) => format!("delete an event in {}", c.label),
            Request::Cal(C::MakeCalendar(c, ..)) => format!("make a calendar in {}", c.label),
        }
    }
}

fn count(n: usize) -> String {
    if n == 1 { "1 message".into() } else { format!("{n} messages") }
}

impl Event {
    /// Why it failed, if it did; else what it did, where that is worth a line.
    fn outcome(&self) -> Result<String, String> {
        use crate::calendar::CalEvent as C;
        fn r<T>(x: &Result<T, String>) -> Result<String, String> {
            x.as_ref().map(|_| String::new()).map_err(|e| e.clone())
        }
        match self {
            Event::SmtpTested(_, x) | Event::ImapTested(_, x) | Event::AccountSmtpTested(_, x) | Event::DraftDeleted(x) | Event::Flagged(x) => r(x),
            Event::Downloaded(_, x) => r(x),
            Event::Categorised(note) => note.as_ref().map_or(Ok(String::new()), |n| Err(n.clone())),
            Event::Done(x) | Event::Sent(x) | Event::Moved(x) | Event::DraftSaved(_, x) => r(x),
            Event::Discovered(_, x) => r(x),
            Event::Synced(_, x) | Event::FolderRefreshed(_, _, x) => x.as_ref().map_err(|e| e.clone()).map(|(rep, rules)| {
                let mut s = format!(": {} folders, {} new, {} gone", rep.folders, rep.new_messages, rep.removed);
                if !rep.reset_folders.is_empty() {
                    s += &format!(", {} reset", rep.reset_folders.len());
                }
                if let Some(e) = &rep.outline_error {
                    s += &format!(", outlines stopped: {e}");
                }
                if let Some(n) = rules {
                    s += &format!(", rules: {n}");
                }
                s
            }),
            Event::Opened(_, _, _, x) => x.as_ref().map(|b| format!(": {} KB", b.len() / 1024)).map_err(|e| e.clone()),
            Event::SignedIn(_, x) => r(x),
            Event::SearchProgress(..) | Event::Downloading(..) => Ok(String::new()),
            Event::Searched(_, _, x) => r(x),
            Event::Quota(_, x) => r(x),
            Event::Cal(C::SignedIn(_, x)) => r(x),
            Event::Cal(C::Tested(_, x)) => r(x),
            Event::Cal(C::Synced(_, x)) => x
                .as_ref()
                .map(|(_, rep)| format!(": {} calendars, {} fetched, {} gone", rep.calendars, rep.fetched, rep.removed))
                .map_err(|e| e.clone()),
            Event::Cal(C::Saved(x, _)) => r(x),
        }
    }
}

/// Logs how a request went, and how long it took.
fn log_outcome(what: &str, began: Instant, ev: &Event) {
    let ms = began.elapsed().as_millis();
    match ev.outcome() {
        Ok(detail) => log::info!("{what}: done in {ms} ms{detail}"),
        Err(e) => log::warn!("{what}: failed after {ms} ms: {e}"),
    }
}

/// The request the worker is on, for the watchdog.
type Busy = Arc<Mutex<Option<(String, Instant)>>>;

/// Every minute a request runs, the log says so: a request that never comes back holds up
/// everything queued behind it, and this is how that shows.
fn watchdog(busy: Busy) {
    std::thread::spawn(move || {
        let mut said: Option<(String, u64)> = None;
        loop {
            std::thread::sleep(Duration::from_secs(15));
            let Ok(g) = busy.lock() else { return };
            let Some((what, since)) = g.as_ref() else {
                said = None;
                continue;
            };
            let mins = since.elapsed().as_secs() / 60;
            if mins >= 1 && said.as_ref().is_none_or(|(w, m)| w != what || *m != mins) {
                log::warn!("{what}: still waiting after {mins} min; everything queued behind it waits too");
                said = Some((what.clone(), mins));
            }
        }
    });
}

pub struct Worker {
    tx: Sender<(Instant, Request)>,
    pub rx: Receiver<Event>,
}

impl Worker {
    /// Queue a request, noting when it was asked for: the log says how long it waited.
    /// False when the worker has stopped.
    pub fn send(&self, r: Request) -> bool {
        self.tx.send((Instant::now(), r)).is_ok()
    }
}

pub fn start(ctx: egui::Context) -> Worker {
    let (tx, req_rx) = channel::<(Instant, Request)>();
    let (ev_tx, rx) = channel::<Event>();
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_multi_thread().enable_all().build() else { return };
        let store = Store::open(crate::accounts::cache_path()).ok();
        // Which server search is wanted: each new one (or a cancel) moves it on, and a search
        // whose number is no longer current stops.
        let search_gen = Arc::new(AtomicU64::new(0));
        // The same for downloading, and which accounts are downloading now (one run each).
        let download_gen = Arc::new(AtomicU64::new(0));
        let downloading: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let busy: Busy = Arc::new(Mutex::new(None));
        watchdog(busy.clone());
        while let Ok((asked, req)) = req_rx.recv() {
            let what = req.describe();
            let began = Instant::now();
            let waited = began.duration_since(asked);
            if waited > Duration::from_millis(200) {
                log::info!("{what}: waited {} ms in the queue", waited.as_millis());
            }
            if let Request::StopDownload = req {
                download_gen.fetch_add(1, Ordering::SeqCst);
                continue;
            }
            // Thousands of messages: its own thread and connection, like search.
            if let Request::Download(acct, password, since) = req {
                let mine = download_gen.load(Ordering::SeqCst);
                let started = downloading.lock().map(|mut d| {
                    let new = !d.contains(&acct.email);
                    if new {
                        d.push(acct.email.clone());
                    }
                    new
                });
                if started.ok() != Some(true) {
                    continue;
                }
                let (tx, ctx, current, running) = (ev_tx.clone(), ctx.clone(), download_gen.clone(), downloading.clone());
                std::thread::spawn(move || {
                    let r = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(rt) => {
                            let store = Store::open(crate::accounts::cache_path()).ok();
                            let live = || current.load(Ordering::SeqCst) == mine;
                            let progress = |here, all| {
                                let _ = tx.send(Event::Downloading(acct.email.clone(), here, all));
                                ctx.request_repaint();
                            };
                            rt.block_on(download(store.as_ref(), &acct, &password, since.as_deref(), &live, &progress))
                        }
                        Err(e) => Err(e.to_string()),
                    };
                    if let Ok(mut d) = running.lock() {
                        d.retain(|e| *e != acct.email);
                    }
                    let ev = Event::Downloaded(acct.email.clone(), r);
                    log_outcome(&what, began, &ev);
                    let _ = tx.send(ev);
                    ctx.request_repaint();
                });
                continue;
            }
            if let Request::CancelSearch = req {
                search_gen.fetch_add(1, Ordering::SeqCst);
                continue;
            }
            // A search of every folder on a server without a text index can take minutes: on its
            // own thread and connection, so opening and syncing do not wait behind it.
            if let Request::Search(acct, password, scope, words) = req {
                let mine = search_gen.fetch_add(1, Ordering::SeqCst) + 1;
                let (tx, ctx, current) = (ev_tx.clone(), ctx.clone(), search_gen.clone());
                std::thread::spawn(move || {
                    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
                    let store = Store::open(crate::accounts::cache_path()).ok();
                    let live = || current.load(Ordering::SeqCst) == mine;
                    let progress = |done, all| {
                        if live() {
                            let _ = tx.send(Event::SearchProgress(done, all));
                            ctx.request_repaint();
                        }
                    };
                    let r = rt.block_on(search(store.as_ref(), &acct, &password, scope.as_deref(), &words, &live, &progress));
                    if let Some(r) = r.filter(|_| live()) {
                        let ev = Event::Searched(scope, words, r);
                        log_outcome(&what, began, &ev);
                        let _ = tx.send(ev);
                        ctx.request_repaint();
                    }
                });
                continue;
            }
            // Signing in waits on the person (minutes, maybe): never hold up mail for it.
            if let Request::SignIn(acct) = req {
                let (tx, ctx) = (ev_tx.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let ev = Event::SignedIn(acct.email.clone(), sign_in(&acct));
                    log_outcome(&what, began, &ev);
                    let _ = tx.send(ev);
                    ctx.request_repaint();
                });
                continue;
            }
            if let Request::CalSignIn(src) = req {
                let (tx, ctx) = (ev_tx.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let ev = Event::Cal(crate::calendar::CalEvent::SignedIn(src.id.clone(), crate::calendar::sign_in(&src)));
                    log_outcome(&what, began, &ev);
                    let _ = tx.send(ev);
                    ctx.request_repaint();
                });
                continue;
            }
            if let Ok(mut b) = busy.lock() {
                *b = Some((what.clone(), began));
            }
            let ev = rt.block_on(async {
                Some(match req {
                    Request::Discover(email) => {
                        let r = mailcraft_autoconfig::discover(&email).await.map_err(|e| e.to_string());
                        Event::Discovered(email, r)
                    }
                    Request::Sync(acct, password) => {
                        let r = sync(store.as_ref(), &acct, &password).await;
                        Event::Synced(acct.email.clone(), r)
                    }
                    Request::Send(acct, password, out, replied, draft, via) => {
                        Event::Sent(send(store.as_ref(), &acct, &password, &out, replied, draft, via.as_deref()).await)
                    }
                    Request::TestImap(acct, password) => Event::ImapTested(
                        acct.email.clone(),
                        // A fresh sign-in, whatever is kept open: that is what is being tested.
                        match imap(&acct, &password).await {
                            Ok(mut s) => {
                                let _ = s.logout().await;
                                Ok(())
                            }
                            Err(e) => Err(e),
                        },
                    ),
                    Request::TestSmtpAccount(acct, secret) => Event::AccountSmtpTested(acct.email.clone(), smtp_check(&acct, &secret).await),
                    Request::TestSmtp(email, via) => Event::SmtpTested(
                        email,
                        async {
                            let cred = via_login(&via).await?;
                            mailcraft_core::send::smtp_test(&via.endpoint, &via.username, cred.login())
                                .await
                                .map_err(|e| format!("{e} (via {}:{} as {})", via.endpoint.host, via.endpoint.port, via.username))
                        }
                        .await,
                    ),
                    Request::SendOnly(acct, password, out) => Event::Done(send_only(&acct, &password, &out).await),
                    Request::SaveDraft(acct, password, out) => {
                        let id = out.message_id.clone().unwrap_or_default();
                        Event::DraftSaved(id, save_draft(store.as_ref(), &acct, &password, &out).await)
                    }
                    Request::DeleteDraft(acct, password, id) => Event::DraftDeleted(delete_draft(store.as_ref(), &acct, &password, &id).await),
                    Request::SignIn(_)
                    | Request::CalSignIn(_)
                    | Request::CancelSearch
                    | Request::Search(..)
                    | Request::Download(..)
                    | Request::StopDownload => return None,
                    Request::Delete(acct, password, folder, uids) => Event::Moved(delete(store.as_ref(), &acct, &password, &folder, &uids).await),
                    Request::Move(acct, password, from, uids, to) => Event::Moved(move_to(store.as_ref(), &acct, &password, &from, &uids, &to).await),
                    Request::Restore(acct, password, r) => Event::Moved(restore(store.as_ref(), &acct, &password, &r).await),
                    Request::RefreshFolder(acct, password, folder) => {
                        let r = refresh_folder(store.as_ref(), &acct, &password, &folder).await;
                        Event::FolderRefreshed(acct.email.clone(), folder, r)
                    }
                    Request::Categorise(acct, password, groups, key, on) => {
                        Event::Categorised(categorise(store.as_ref(), &acct, &password, &groups, &key, on).await)
                    }
                    Request::RunRules(acct, password, folder) => Event::Done(run_rules(store.as_ref(), &acct, &password, &folder).await),
                    // Emptying or marking read changes only messages: the screen already
                    // shows it, and the folders it touched are refreshed, not every folder.
                    Request::Folder(acct, password, op) => {
                        let messages_only = op.messages_only();
                        let r = folder_op(store.as_ref(), &acct, &password, op).await;
                        if messages_only { Event::Moved(r) } else { Event::Done(r) }
                    }
                    Request::MoveSpecial(acct, password, from, uids, which) => {
                        Event::Moved(move_special(store.as_ref(), &acct, &password, &from, &uids, which).await)
                    }
                    Request::Copy(acct, password, from, uids, to) => Event::Moved(copy(store.as_ref(), &acct, &password, &from, &uids, &to).await),
                    Request::SetFlag(acct, password, folder, uids, flag, on) => {
                        Event::Flagged(set_flag(store.as_ref(), &acct, &password, &folder, &uids, &flag, on).await)
                    }
                    Request::Quota(acct, password) => Event::Quota(acct.email.clone(), quota(&acct, &password).await),
                    Request::Cal(r) => Event::Cal(crate::calendar::run(store.as_ref(), r)),
                    Request::Flag(acct, password, folder, uid, flag) => {
                        Event::Flagged(flag_one(store.as_ref(), &acct, &password, &folder, uid, &flag).await)
                    }
                    Request::Open(acct, password, folder, uid) => {
                        let r = open(store.as_ref(), &acct, &password, &folder, uid).await;
                        Event::Opened(acct.email.clone(), folder, uid, r)
                    }
                })
            });
            if let Ok(mut b) = busy.lock() {
                *b = None;
            }
            let Some(ev) = ev else { continue };
            log_outcome(&what, began, &ev);
            if ev_tx.send(ev).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
    Worker { tx, rx }
}

/// The account's enabled rules, in order.
fn rules_for(acct: &Account) -> Vec<mailcraft_core::rules::Rule> {
    crate::accounts::load_rules().into_iter().filter(|r| r.enabled && r.account == acct.email).collect()
}

/// The Inbox's cache id, UIDVALIDITY and highest UID before a sync.
fn inbox_state(store: &Store, account: &str) -> Option<(i64, Option<u32>, u32)> {
    let f = store.folders(account).ok()?.into_iter().find(|f| f.special.as_deref() == Some("\\Inbox") || f.name.eq_ignore_ascii_case("INBOX"))?;
    let (v, max) = store.sync_state(f.id).ok()?;
    Some((f.id, v, max))
}

async fn sync(store: Option<&Store>, acct: &Account, password: &str) -> Result<(mailcraft_core::sync::Report, Option<String>), String> {
    let store = store.ok_or("the local cache could not be opened")?;
    let mut s = conn(acct, password).await?;
    let before = inbox_state(store, &acct.email);
    let rep = mailcraft_core::sync::sync_account(&mut s, store, &acct.email).await.map_err(|e| e.to_string());
    let note = if rep.is_ok() { new_mail_rules(&mut s, store, acct, before).await } else { None };
    // Restore's records of mail that has left Trash some other way (a day's grace: a message
    // just deleted may not be in this PC's copy of Trash yet).
    if rep.is_ok()
        && let Err(e) = store.prune_origins(&acct.email, 86_400)
    {
        log::warn!("{}: forgetting where old Trash came from: {e}", acct.email);
    }
    keep_conn(acct, s);
    rep.map(|r| (r, note))
}

/// Rules act on mail that arrived since the last sync, as Outlook's do: never on the first
/// sync of an account (everything would look new), nor when the Inbox was reset. `before` is
/// the Inbox as it was before the sync (or the Inbox's refresh); what the rules did, if anything.
async fn new_mail_rules(
    s: &mut mailcraft_core::connect::Session,
    store: &Store,
    acct: &Account,
    before: Option<(i64, Option<u32>, u32)>,
) -> Option<String> {
    let mut note = None;
    if let Some((id, validity, max)) = before
        && max > 0
        && inbox_state(store, &acct.email).is_some_and(|(_, v, _)| v == validity)
    {
        let rules = rules_for(acct);
        let prefs = crate::settings::load();
        if !rules.is_empty() || !prefs.blocked.is_empty() {
            let mut new: Vec<_> = store.messages(id, 5000).unwrap_or_default().into_iter().filter(|m| m.uid > max).collect();
            let folder = new.first().map(|m| m.folder.clone()).unwrap_or_else(|| "INBOX".into());
            let mut notes = vec![];
            // Blocked senders first (Settings > Junk email): straight to Junk, unless also safe.
            let blocked: Vec<u32> = new
                .iter()
                .filter(|m| {
                    let from = m.from_addr.as_deref().unwrap_or("");
                    crate::settings::Settings::listed(&prefs.blocked, from) && !crate::settings::Settings::listed(&prefs.safe, from)
                })
                .map(|m| m.uid)
                .collect();
            if !blocked.is_empty() {
                let moved = async {
                    let junk = mailcraft_core::manage::special_folder(s, store, &acct.email, mailcraft_core::manage::Special::Junk).await?;
                    mailcraft_core::manage::move_messages(s, &folder, &blocked, &junk).await?;
                    Ok::<_, mailcraft_core::Error>(junk)
                }
                .await;
                match moved {
                    Ok(junk) => notes.push(format!("{} from blocked senders moved to {junk}", blocked.len())),
                    Err(e) => notes.push(format!("blocked senders not moved: {e}")),
                }
                new.retain(|m| !blocked.contains(&m.uid));
            }
            let plan = mailcraft_core::rules::plan(&rules, &new, &folder);
            if !plan.is_empty() {
                notes.push(execute(s, store, acct, &folder, &plan).await);
            }
            if !notes.is_empty() {
                note = Some(notes.join("; "));
                // Show where things went.
                let _ = mailcraft_core::sync::sync_account(s, store, &acct.email).await;
            }
        }
    }
    note
}

/// Bring one folder up to date (F5) on the kept connection. The Inbox's new mail goes through
/// the rules as a full sync's would: otherwise the next sync would see it as old and skip it.
async fn refresh_folder(
    store: Option<&Store>,
    acct: &Account,
    password: &str,
    folder: &str,
) -> Result<(mailcraft_core::sync::Report, Option<String>), String> {
    let store = store.ok_or("the local cache could not be opened")?;
    let mut s = conn(acct, password).await?;
    let before = inbox_state(store, &acct.email);
    let rep = mailcraft_core::sync::sync_folders(&mut s, store, &acct.email, &[folder.to_string()]).await.map_err(|e| e.to_string());
    let is_inbox = before.is_some_and(|(id, _, _)| store.folder_id(&acct.email, folder).ok().flatten() == Some(id));
    let note = if rep.is_ok() && is_inbox { new_mail_rules(&mut s, store, acct, before).await } else { None };
    keep_conn(acct, s);
    rep.map(|r| (r, note))
}

/// Do what a rules plan says, best effort: one failed step does not stop the others.
async fn execute(s: &mut mailcraft_core::connect::Session, store: &Store, acct: &Account, folder: &str, p: &mailcraft_core::rules::Plan) -> String {
    use mailcraft_core::manage;
    let mut done = vec![];
    let mut failed = vec![];
    let count = |n: usize, what: &str| format!("{n} {what}");
    if !p.mark_read.is_empty() {
        match manage::set_flag(s, folder, &p.mark_read, "\\Seen", true).await {
            Ok(()) => done.push(count(p.mark_read.len(), "marked read")),
            Err(e) => failed.push(e.to_string()),
        }
    }
    if !p.flag.is_empty() {
        match manage::set_flag(s, folder, &p.flag, "\\Flagged", true).await {
            Ok(()) => done.push(count(p.flag.len(), "flagged")),
            Err(e) => failed.push(e.to_string()),
        }
    }
    for (key, uids) in &p.categorise {
        let on_server = manage::set_flag(s, folder, uids, key, true).await.is_ok();
        keep_categories(store, acct, folder, uids, key, true);
        done.push(format!("{} categorised{}", uids.len(), if on_server { "" } else { " (on this computer)" }));
    }
    for (to, uids) in &p.copy {
        match manage::copy_messages(s, folder, uids, to).await {
            Ok(()) => done.push(format!("{} copied to {to}", uids.len())),
            Err(e) => failed.push(format!("copy to {to}: {e}")),
        }
    }
    for (to, uids) in &p.moves {
        match manage::move_messages(s, folder, uids, to).await {
            Ok(()) => done.push(format!("{} moved to {to}", uids.len())),
            Err(e) => failed.push(format!("move to {to}: {e}")),
        }
    }
    if !p.delete.is_empty() {
        match manage::delete_messages(s, store, &acct.email, folder, &p.delete).await {
            Ok(_) => done.push(count(p.delete.len(), "deleted")),
            Err(e) => failed.push(e.to_string()),
        }
    }
    let mut out = format!("Rules: {}", done.join(", "));
    if !failed.is_empty() {
        out.push_str(&format!(" (not done: {})", failed.join("; ")));
    }
    out
}

async fn run_rules(store: Option<&Store>, acct: &Account, password: &str, folder: &str) -> Result<String, String> {
    let store = store.ok_or("the local cache could not be opened")?;
    let rules = rules_for(acct);
    if rules.is_empty() {
        return Err("this account has no rules switched on".into());
    }
    let id = store.folder_id(&acct.email, folder).map_err(|e| e.to_string())?.ok_or("that folder is not in the cache yet; sync first")?;
    let all = store.messages(id, 100_000).map_err(|e| e.to_string())?;
    let plan = mailcraft_core::rules::plan(&rules, &all, folder);
    if plan.is_empty() {
        return Ok(format!("Rules: nothing in {folder} matched."));
    }
    let mut s = conn(acct, password).await?;
    let out = execute(&mut s, store, acct, folder, &plan).await;
    keep_conn(acct, s);
    Ok(format!("{out} ({} of {} messages in {folder} matched).", plan.matched, all.len()))
}

async fn open(store: Option<&Store>, acct: &Account, password: &str, folder: &str, uid: u32) -> Result<Vec<u8>, String> {
    let store = store.ok_or("the local cache could not be opened")?;
    if let Some(id) = store.folder_id(&acct.email, folder).map_err(|e| e.to_string())?
        && let Some(raw) = store.body(id, uid).map_err(|e| e.to_string())?
    {
        return Ok(raw);
    }
    let mut s = conn(acct, password).await?;
    let raw = mailcraft_core::sync::fetch_body(&mut s, store, &acct.email, folder, uid).await.map_err(|e| e.to_string());
    keep_conn(acct, s);
    raw
}

async fn send(
    store: Option<&Store>,
    acct: &Account,
    password: &str,
    out: &mailcraft_core::send::Outgoing,
    replied: Option<(String, u32)>,
    draft: Option<String>,
    via: Option<&Via>,
) -> Result<String, String> {
    use mailcraft_core::send;
    let msg = send::build(out).map_err(|e| e.to_string())?;
    match via {
        // A send-only address: its own server; the copy still goes in this account's Sent.
        Some(v) => {
            let cred = via_login(v).await?;
            send::smtp_send(&v.endpoint, &v.username, cred.login(), &msg)
                .await
                .map_err(|e| format!("{e} (via {}:{} as {})", v.endpoint.host, v.endpoint.port, v.username))?;
        }
        None => {
            let smtp = acct.smtp_endpoint().ok_or("this account has no outgoing (SMTP) server")?;
            let cred = credential(acct, password).await?;
            if let Err(e) = send::smtp_send(&smtp, &acct.username, cred.login(), &msg).await {
                forget_token(acct, &e);
                // Say where it went and as whom: what is needed to put it right.
                return Err(format!("{e} (via {}:{} as {})", smtp.host, smtp.port, acct.username));
            }
        }
    }
    // Sent. Keeping a copy and flagging the original are nice to have: report, don't fail.
    let mut notes = vec![];
    match (store, conn(acct, password).await) {
        (Some(store), Ok(mut s)) => {
            match send::sent_folder(&mut s, store, &acct.email).await {
                Ok(folder) => {
                    if let Err(e) = send::append(&mut s, &folder, &msg.formatted()).await {
                        notes.push(format!("no copy saved in {folder}: {e}"));
                    }
                }
                Err(e) => notes.push(format!("no Sent folder: {e}")),
            }
            if let Some((folder, uid)) = replied
                && send::store_flags(&mut s, &folder, uid, "+FLAGS", "\\Answered").await.is_ok()
                && let Ok(Some(id)) = store.folder_id(&acct.email, &folder)
            {
                let _ = store.add_flag(id, uid, "\\Answered");
            }
            if let Some(id) = draft
                && let Err(e) = send::delete_draft(&mut s, store, &acct.email, &id).await
            {
                notes.push(format!("the draft is still in Drafts: {e}"));
            }
            keep_conn(acct, s);
        }
        (_, Err(e)) => notes.push(format!("no copy saved: {e}")),
        (None, _) => notes.push("no copy saved: the local cache is not open".into()),
    }
    Ok(if notes.is_empty() { "Sent.".into() } else { format!("Sent ({})", notes.join("; ")) })
}

/// Sign in to the account's outgoing server as sending would (password or the provider's
/// token), sending nothing.
async fn smtp_check(acct: &Account, secret: &str) -> Result<(), String> {
    let smtp = acct.smtp_endpoint().ok_or("this account has no outgoing (SMTP) server")?;
    let cred = credential(acct, secret).await?;
    mailcraft_core::send::smtp_test(&smtp, &acct.username, cred.login())
        .await
        .map_err(|e| format!("{e} (via {}:{} as {})", smtp.host, smtp.port, acct.username))
}

async fn quota(acct: &Account, password: &str) -> Result<Option<(u64, u64)>, String> {
    let mut s = conn(acct, password).await?;
    // A server without the QUOTA extension refuses the command: that just means no figure.
    let r = mailcraft_core::manage::quota(&mut s).await.unwrap_or(None);
    keep_conn(acct, s);
    Ok(r)
}

async fn flag_one(store: Option<&Store>, acct: &Account, password: &str, folder: &str, uid: u32, flag: &str) -> Result<(), String> {
    let mut s = conn(acct, password).await?;
    let r = mailcraft_core::send::store_flags(&mut s, folder, uid, "+FLAGS", flag).await.map_err(|e| e.to_string());
    keep_conn(acct, s);
    r?;
    if let Some(store) = store
        && let Ok(Some(id)) = store.folder_id(&acct.email, folder)
    {
        store.add_flag(id, uid, flag).map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn save_draft(store: Option<&Store>, acct: &Account, password: &str, out: &mailcraft_core::send::Outgoing) -> Result<String, String> {
    use mailcraft_core::send;
    let store = store.ok_or("the local cache could not be opened")?;
    let id = out.message_id.as_deref().ok_or("a draft needs a Message-ID")?;
    let raw = send::build_draft(out).map_err(|e| e.to_string())?.formatted();
    let mut s = conn(acct, password).await?;
    let r = send::save_draft(&mut s, store, &acct.email, &raw, id).await.map_err(|e| e.to_string());
    keep_conn(acct, s);
    r
}

async fn delete_draft(store: Option<&Store>, acct: &Account, password: &str, id: &str) -> Result<(), String> {
    let store = store.ok_or("the local cache could not be opened")?;
    let mut s = conn(acct, password).await?;
    let r = mailcraft_core::send::delete_draft(&mut s, store, &acct.email, id).await.map_err(|e| e.to_string());
    keep_conn(acct, s);
    r
}

/// Errors that only signing in again can fix start with this; the window then offers the sign-in.
pub const SIGN_IN_AGAIN: &str = "Sign in again: ";

/// Access tokens in hand, per address, with when they stop working. Never written to disk.
static TOKENS: Mutex<Option<HashMap<String, (String, Instant)>>> = Mutex::new(None);

enum Credential {
    Password(String),
    Token(String),
}

impl Credential {
    fn login(&self) -> mailcraft_core::Login<'_> {
        match self {
            Credential::Password(p) => mailcraft_core::Login::Password(p),
            Credential::Token(t) => mailcraft_core::Login::OAuth2(t),
        }
    }
}

/// What to log in with. For OAuth accounts `secret` is the refresh token: use the access token
/// in hand, or fetch a new one (keeping the new refresh token when the provider rotates it).
async fn credential(acct: &Account, secret: &str) -> Result<Credential, String> {
    let Some(p) = acct.oauth else { return Ok(Credential::Password(secret.to_string())) };
    if let Some((t, until)) = TOKENS.lock().ok().and_then(|m| m.as_ref().and_then(|m| m.get(&acct.email).cloned()))
        && Instant::now() < until
    {
        return Ok(Credential::Token(t));
    }
    let client = crate::accounts::oauth_client(p).ok_or_else(|| format!("{SIGN_IN_AGAIN}MailCraft has no {} app registration", p.name()))?;
    // The saved token, not the copy this request carried: an earlier step may have rotated it.
    let refresh = crate::secrets::get(&acct.email).unwrap_or_else(|| secret.to_string());
    let refresh_used = refresh.clone();
    let got = tokio::task::spawn_blocking(move || oauth::refresh(p, &client, &refresh)).await.map_err(|e| e.to_string())?;
    match got {
        Ok(t) => {
            keep(acct, p, &t, &refresh_used);
            Ok(Credential::Token(t.access_token))
        }
        Err(e) if oauth::is_revoked(&e) => Err(format!("{SIGN_IN_AGAIN}{} no longer accepts the saved sign-in ({e})", p.name())),
        Err(e) => Err(e.to_string()),
    }
}

/// Remember new tokens: the access token in memory, a new refresh token in the keyring.
fn keep(acct: &Account, _p: Provider, t: &oauth::Tokens, old_refresh: &str) {
    if let Some(r) = t.refresh_token.as_deref().filter(|r| *r != old_refresh) {
        crate::secrets::set(&acct.email, r);
    }
    let until = Instant::now() + t.expires_in.saturating_sub(Duration::from_secs(120));
    if let Ok(mut m) = TOKENS.lock() {
        m.get_or_insert_with(HashMap::new).insert(acct.email.clone(), (t.access_token.clone(), until));
    }
}

/// A refused login may mean the access token was revoked early: get a fresh one next time.
fn forget_token(acct: &Account, e: &mailcraft_core::Error) {
    if acct.oauth.is_some()
        && matches!(e, mailcraft_core::Error::Login(_) | mailcraft_core::Error::Send(_))
        && let Ok(mut m) = TOKENS.lock()
        && let Some(m) = m.as_mut()
    {
        m.remove(&acct.email);
    }
}

async fn imap(acct: &Account, secret: &str) -> Result<mailcraft_core::connect::Session, String> {
    let cred = credential(acct, secret).await?;
    mailcraft_core::connect_with(&acct.imap_endpoint(), &acct.username, cred.login()).await.map_err(|e| {
        forget_token(acct, &e);
        e.to_string()
    })
}

// The main queue's connection, kept open between requests (2026-10-29: signing in for
// every one was 21 sign-ins in three minutes). One per account; only the main queue uses it,
// because a connection belongs to the runtime that made it (search and downloading have
// their own threads and connections). Each reuse is checked with a NOOP first, so one the
// server closed while idle, or one an error left in a bad way, is replaced, not used.
std::thread_local! {
    static KEPT: std::cell::RefCell<HashMap<String, (mailcraft_core::connect::Session, Instant)>> = std::cell::RefCell::new(HashMap::new());
}

/// How long a reused connection has to answer NOOP before a new one is made instead.
const NOOP_WAIT: Duration = Duration::from_secs(10);

/// A connection idle longer than this is replaced, not tried: servers and routers close idle
/// ones in their own time (the GreenMail test server after about 40 s, Dovecot after 30 min),
/// and one closed just after its NOOP would fail the request it was kept for.
const KEEP_FOR: Duration = Duration::from_secs(120);

/// The account's kept connection, else a new one.
async fn conn(acct: &Account, secret: &str) -> Result<mailcraft_core::connect::Session, String> {
    let key = kept_key(acct);
    if let Some((mut s, since)) = KEPT.with(|k| k.borrow_mut().remove(&key)).filter(|(_, since)| since.elapsed() < KEEP_FOR) {
        match tokio::time::timeout(NOOP_WAIT, s.noop()).await {
            Ok(Ok(())) => {
                log::debug!("{}: reusing the kept connection (idle {} ms)", acct.email, since.elapsed().as_millis());
                return Ok(s);
            }
            Ok(Err(e)) => {
                log::info!("{}: the kept connection is gone after {} ms idle ({e}); connecting again", acct.email, since.elapsed().as_millis())
            }
            Err(_) => log::info!("{}: the kept connection did not answer; connecting again", acct.email),
        }
    }
    imap(acct, secret).await
}

/// Keep a connection for the account's next request.
fn keep_conn(acct: &Account, s: mailcraft_core::connect::Session) {
    let key = kept_key(acct);
    KEPT.with(|k| k.borrow_mut().insert(key, (s, Instant::now())));
}

/// The account and where and as whom it connects: a change in settings gets a new connection.
fn kept_key(acct: &Account) -> String {
    let ep = acct.imap_endpoint();
    format!("{}|{}|{}:{}", acct.email, acct.username, ep.host, ep.port)
}

/// The browser sign-in. On success the refresh token is in the keyring and an access token in hand.
fn sign_in(acct: &Account) -> Result<Option<String>, String> {
    let p = acct.oauth.ok_or("this account signs in with a password")?;
    let client = crate::accounts::oauth_client(p).ok_or_else(|| format!("MailCraft has no {} app registration yet", p.name()))?;
    let t = oauth::sign_in(p, &client, &acct.email, open_browser).map_err(|e| e.to_string())?;
    keep(acct, p, &t, "");
    Ok(t.email)
}

pub(crate) fn open_browser(url: &str) -> Result<(), String> {
    // Automated tests: no browser; fetch the page and follow its redirects back to us.
    if std::env::var_os("MAILCRAFT_TEST_OAUTH_BROWSER").is_some() {
        let url = url.to_string();
        std::thread::spawn(move || {
            let _ = ureq::get(&url).call();
        });
        return Ok(());
    }
    open::that_detached(url).map_err(|e| e.to_string())
}

async fn delete(store: Option<&Store>, acct: &Account, password: &str, folder: &str, uids: &[u32]) -> Result<String, String> {
    use mailcraft_core::manage::{Deleted, delete_messages};
    let store = store.ok_or("the local cache could not be opened")?;
    let mut s = conn(acct, password).await?;
    let r = delete_messages(&mut s, store, &acct.email, folder, uids).await.map_err(|e| e.to_string());
    match &r {
        Ok(Deleted::MovedTo(t)) => refresh_touched(&mut s, Some(store), acct, &[folder, t]).await,
        Ok(Deleted::Gone) => refresh_touched(&mut s, Some(store), acct, &[folder]).await,
        Err(_) => {}
    }
    keep_conn(acct, s);
    let n = if uids.len() == 1 { "Message".to_string() } else { format!("{} messages", uids.len()) };
    Ok(match r? {
        Deleted::MovedTo(t) => format!("{n} moved to {t}."),
        Deleted::Gone => format!("{n} deleted for good."),
    })
}

async fn move_to(store: Option<&Store>, acct: &Account, password: &str, from: &str, uids: &[u32], to: &str) -> Result<String, String> {
    let mut s = conn(acct, password).await?;
    let r = mailcraft_core::manage::move_messages(&mut s, from, uids, to).await.map_err(|e| e.to_string());
    if r.is_ok() {
        refresh_touched(&mut s, store, acct, &[from, to]).await;
    }
    keep_conn(acct, s);
    r?;
    Ok(if uids.len() == 1 { format!("Message moved to {to}.") } else { format!("{} messages moved to {to}.", uids.len()) })
}

/// Restore: move back from Trash, the same way as any move; where they came from is
/// forgotten only once the server has them back (a failure leaves them in Trash, still known).
async fn restore(store: Option<&Store>, acct: &Account, password: &str, r: &Restore) -> Result<String, String> {
    move_to(store, acct, password, &r.trash, &r.uids, &r.to).await?;
    if let Some(store) = store
        && let Err(e) = store.forget_origins(&acct.email, &r.message_ids)
    {
        log::warn!("{}: forgetting where restored mail came from: {e}", acct.email);
    }
    let n = r.uids.len();
    let what = if n == 1 { "Message".to_string() } else { format!("{n} messages") };
    Ok(match r.unknown {
        0 => format!("{what} restored to {}.", r.label),
        u if u >= n => format!("{what} restored to {}: where {} deleted from is not known.", r.label, if n == 1 { "it was" } else { "they were" }),
        u => format!("{what} restored to {} (for {u} of them, where they were deleted from is not known).", r.label),
    })
}

/// Bring the folders a change touched up to date on the same connection, so the folder tree
/// can show it without a sync of every folder. Failing here is only logged: the next sync
/// catches up anyway.
async fn refresh_touched(s: &mut mailcraft_core::connect::Session, store: Option<&Store>, acct: &Account, folders: &[&str]) {
    let Some(store) = store else { return };
    let mut names: Vec<String> = folders.iter().map(|f| f.to_string()).collect();
    names.dedup();
    if let Err(e) = mailcraft_core::sync::sync_folders(s, store, &acct.email, &names).await {
        log::warn!("{}: refreshing {} after the change: {e}", acct.email, names.join(", "));
    }
}

/// Download what the cache lacks, newest first, a folder's batch at a time; messages
/// downloaded before the search index existed are indexed first. Stops when `live` turns false.
/// A batch that fails (a folder that cannot be opened, say) is skipped, and the last error is
/// reported once the rest is done.
async fn download(
    store: Option<&Store>,
    acct: &Account,
    password: &str,
    since: Option<&str>,
    live: &dyn Fn() -> bool,
    progress: &dyn Fn(i64, i64),
) -> Result<(), String> {
    let store = store.ok_or("the local cache could not be opened")?;
    let me = acct.email.as_str();
    while live() && store.index_backlog(me, 200).map_err(|e| e.to_string())? > 0 {}
    let say = || {
        if let Ok((here, all)) = store.downloaded(me, None) {
            progress(here, all);
        }
    };
    say();
    let mut tried: std::collections::HashSet<(String, u32)> = std::collections::HashSet::new();
    let mut session = None;
    let mut last_error = None;
    while live() {
        let todo: Vec<(String, u32)> =
            store.missing_bodies(me, since, tried.len() + 200).map_err(|e| e.to_string())?.into_iter().filter(|t| !tried.contains(t)).collect();
        if todo.is_empty() {
            break;
        }
        let s = match &mut session {
            Some(s) => s,
            None => session.insert(imap(acct, password).await?),
        };
        // In date order, a run of the same folder at a time, 20 messages a command.
        let mut i = 0;
        while let Some((folder, _)) = todo.get(i) {
            let run: Vec<u32> = todo.get(i..).unwrap_or(&[]).iter().take_while(|(f, _)| f == folder).take(20).map(|(_, u)| *u).collect();
            i += run.len().max(1);
            if let Err(e) = mailcraft_core::sync::fetch_bodies(s, store, me, folder, &run).await {
                last_error = Some(format!("{folder}: {e}"));
            }
            tried.extend(run.into_iter().map(|u| (folder.clone(), u)));
            say();
            if !live() {
                break;
            }
        }
    }
    if let Some(mut s) = session {
        let _ = s.logout().await;
    }
    last_error.map_or(Ok(()), Err)
}

/// Search the server, folder by folder, saying how far it has got. None when `live` turns
/// false (a newer search, or Stop): the connection is dropped there and then, mid-command.
async fn search(
    store: Option<&Store>,
    acct: &Account,
    password: &str,
    folder: Option<&str>,
    words: &str,
    live: &dyn Fn() -> bool,
    progress: &dyn Fn(usize, usize),
) -> Option<Result<Vec<(String, Vec<u32>)>, String>> {
    let folders: Vec<String> = match folder {
        Some(f) => vec![f.to_string()],
        None => {
            match store.ok_or("the local cache could not be opened".to_string()).and_then(|st| st.folders(&acct.email).map_err(|e| e.to_string())) {
                Ok(fs) => fs.into_iter().map(|f| f.name).collect(),
                Err(e) => return Some(Err(e)),
            }
        }
    };
    let work = async {
        let mut s = imap(acct, password).await?;
        let mut out = vec![];
        for (i, f) in folders.iter().enumerate() {
            progress(i, folders.len());
            if let Ok(uids) = mailcraft_core::manage::search(&mut s, f, words).await
                && !uids.is_empty()
            {
                out.push((f.clone(), uids));
            }
        }
        let _ = s.logout().await;
        Ok(out)
    };
    let stopped = async {
        while live() {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    };
    tokio::select! {
        r = work => Some(r),
        () = stopped => None,
    }
}

async fn folder_op(store: Option<&Store>, acct: &Account, password: &str, op: FolderOp) -> Result<String, String> {
    use mailcraft_core::manage;
    let store = store.ok_or("the local cache could not be opened")?;
    let mut s = conn(acct, password).await?;
    let id = |name: &str| store.folder_id(&acct.email, name).ok().flatten();
    // The folders whose messages changed, brought up to date after on the same connection.
    let mut touched: Vec<String> = vec![];
    let r = match &op {
        FolderOp::Create(name) => manage::create_folder(&mut s, name).await.map(|_| format!("Folder {name} created.")),
        FolderOp::Rename(from, to) => {
            let delim = store.folders(&acct.email).ok().and_then(|fs| fs.into_iter().find(|f| f.name == *from)).and_then(|f| f.delimiter);
            let r = manage::rename_folder(&mut s, from, to).await;
            if r.is_ok() {
                let _ = store.rename_folder(&acct.email, from, to, delim.as_deref());
            }
            r.map(|_| format!("{from} renamed to {to}."))
        }
        FolderOp::Delete(name) => manage::delete_folder(&mut s, name).await.map(|_| format!("Folder {name} deleted.")),
        FolderOp::MarkAllRead(name) => {
            let r = manage::mark_all_read(&mut s, name).await;
            if r.is_ok()
                && let Some(i) = id(name)
            {
                let _ = store.mark_all_read(i);
            }
            touched.push(name.clone());
            r.map(|_| format!("Everything in {name} marked as read."))
        }
        FolderOp::EmptyTrash => match manage::trash_folder(&mut s, store, &acct.email).await {
            Ok(trash) => {
                let r = manage::empty_folder(&mut s, &trash).await;
                if r.is_ok()
                    && let Some(i) = id(&trash)
                {
                    // Emptying on the way out comes straight here, not through the app's copy.
                    let _ = store.forget_origins_in(i, None);
                    let _ = store.clear_folder(i);
                }
                touched.push(trash.clone());
                r.map(|n| format!("{trash} emptied ({n})."))
            }
            Err(e) => Err(e),
        },
        FolderOp::EmptyToTrash(name) => match manage::trash_folder(&mut s, store, &acct.email).await {
            Ok(trash) => {
                let r = manage::move_all(&mut s, name, &trash).await;
                if r.is_ok()
                    && let Some(i) = id(name)
                {
                    let _ = store.clear_folder(i);
                }
                touched.extend([name.clone(), trash.clone()]);
                r.map(|n| format!("{name} emptied: {n} moved to {trash}."))
            }
            Err(e) => Err(e),
        },
        FolderOp::Empty(name) => {
            let r = manage::empty_folder(&mut s, name).await;
            if r.is_ok()
                && let Some(i) = id(name)
            {
                let _ = store.clear_folder(i);
            }
            touched.push(name.clone());
            r.map(|n| format!("{name} emptied ({n} deleted for good)."))
        }
    };
    if r.is_ok() && !touched.is_empty() {
        let names: Vec<&str> = touched.iter().map(String::as_str).collect();
        refresh_touched(&mut s, Some(store), acct, &names).await;
    }
    keep_conn(acct, s);
    r.map_err(|e| e.to_string())
}

impl FolderOp {
    /// Changes only the messages in folders, not the folders themselves: done on this PC
    /// first, and followed by a refresh of just those folders.
    pub fn messages_only(&self) -> bool {
        matches!(self, FolderOp::MarkAllRead(_) | FolderOp::Empty(_) | FolderOp::EmptyTrash | FolderOp::EmptyToTrash(_))
    }
}

async fn move_special(
    store: Option<&Store>,
    acct: &Account,
    password: &str,
    from: &str,
    uids: &[u32],
    which: mailcraft_core::manage::Special,
) -> Result<String, String> {
    use mailcraft_core::manage;
    let store = store.ok_or("the local cache could not be opened")?;
    let mut s = conn(acct, password).await?;
    let r = async {
        let to = manage::special_folder(&mut s, store, &acct.email, which).await?;
        manage::move_messages(&mut s, from, uids, &to).await?;
        Ok::<_, mailcraft_core::Error>(to)
    }
    .await;
    if let Ok(to) = &r {
        refresh_touched(&mut s, Some(store), acct, &[from, to]).await;
    }
    keep_conn(acct, s);
    let to = r.map_err(|e| e.to_string())?;
    Ok(if uids.len() == 1 { format!("Message moved to {to}.") } else { format!("{} messages moved to {to}.", uids.len()) })
}

async fn copy(store: Option<&Store>, acct: &Account, password: &str, from: &str, uids: &[u32], to: &str) -> Result<String, String> {
    let mut s = conn(acct, password).await?;
    let r = mailcraft_core::manage::copy_messages(&mut s, from, uids, to).await.map_err(|e| e.to_string());
    if r.is_ok() {
        refresh_touched(&mut s, store, acct, &[to]).await;
    }
    keep_conn(acct, s);
    r?;
    Ok(if uids.len() == 1 { format!("Message copied to {to}.") } else { format!("{} messages copied to {to}.", uids.len()) })
}

async fn set_flag(store: Option<&Store>, acct: &Account, password: &str, folder: &str, uids: &[u32], flag: &str, on: bool) -> Result<(), String> {
    let mut s = conn(acct, password).await?;
    let r = mailcraft_core::manage::set_flag(&mut s, folder, uids, flag, on).await.map_err(|e| e.to_string());
    keep_conn(acct, s);
    r?;
    if let Some(store) = store
        && let Ok(Some(id)) = store.folder_id(&acct.email, folder)
    {
        for u in uids {
            let _ = if on { store.add_flag(id, *u, flag) } else { store.remove_flag(id, *u, flag) };
        }
    }
    Ok(())
}

/// Categories always go in the cache; on the server too where it keeps keywords (Gmail,
/// Fastmail, Dovecot do; Outlook.com does not), so other mail apps see them.
async fn categorise(store: Option<&Store>, acct: &Account, password: &str, groups: &[(String, Vec<u32>)], key: &str, on: bool) -> Option<String> {
    let store = store?;
    for (folder, uids) in groups {
        keep_categories(store, acct, folder, uids, key, on);
    }
    let mut refused = false;
    match conn(acct, password).await {
        Ok(mut s) => {
            for (folder, uids) in groups {
                if mailcraft_core::manage::set_flag(&mut s, folder, uids, key, on).await.is_err() {
                    refused = true;
                }
            }
            keep_conn(acct, s);
        }
        Err(_) => refused = true,
    }
    refused.then(|| "Categories are kept on this computer: the server would not store them.".to_string())
}

fn keep_categories(store: &Store, acct: &Account, folder: &str, uids: &[u32], key: &str, on: bool) {
    if let Ok(Some(id)) = store.folder_id(&acct.email, folder) {
        for u in uids {
            let _ = store.set_category(id, *u, key, on);
            // The cached server flags hold the keyword too: keep them in step until the next sync.
            let _ = if on { store.add_flag(id, *u, key) } else { store.remove_flag(id, *u, key) };
        }
    }
}

async fn send_only(acct: &Account, password: &str, out: &mailcraft_core::send::Outgoing) -> Result<String, String> {
    use mailcraft_core::send;
    let smtp = acct.smtp_endpoint().ok_or("this account has no outgoing (SMTP) server")?;
    let msg = send::build(out).map_err(|e| e.to_string())?;
    let cred = credential(acct, password).await?;
    send::smtp_send(&smtp, &acct.username, cred.login(), &msg).await.map_err(|e| e.to_string())?;
    Ok("Read receipt sent.".into())
}
