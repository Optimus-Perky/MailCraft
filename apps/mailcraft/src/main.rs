//! MailCraft desktop app: an Outlook-style three-pane mail window on WordCraft's engine.
//!
//! Folders on the left, the message list in the middle, and the open message on the right,
//! drawn by WordCraft's own canvas (the mail's HTML imported by WordCraft's HTML reader, shown in
//! Web Layout and protected read-only, so it can be selected and copied but not edited).
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod accounts;
mod calendar;
mod darkmail;
mod foldertree;
mod history;
mod htmlview;
mod invite;
mod links;
mod logfile;
mod looks;
mod mailto;
mod notify;
mod people;
mod ribbon;
mod secrets;
mod settings;
mod settings_ui;
mod transfer;
mod worker;

use accounts::{Account, ServerCfg};
use egui::{Rect, RichText, Sense, pos2, vec2};
use mail_parser::MimeHeaders;
use mailcraft_autoconfig::Settings;
use mailcraft_core::oauth::Provider;
use mailcraft_core::{FolderRow, MessageRow, Store};
use ribbon::{Action, Filter, PaneAt, SortKey, Tab};
use wordcraft_engine::Session;
use wordcraft_ui_egui::theme::{Tokens, regular, semibold};
use wordcraft_ui_egui::{Services, WordApp};
use worker::{Event, FolderOp, Request, Worker};

#[derive(Default)]
struct AddDialog {
    email: String,
    password: String,
    client_id: String,
    client_secret: String,
    found: Option<Settings>,
    busy: bool,
    error: Option<String>,
}

/// How the window is laid out, remembered between runs (view.json).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct View {
    tab: Tab,
    ribbon_collapsed: bool,
    ribbon_simplified: bool,
    reading_pane: PaneAt,
    folder_pane: bool,
    sort: SortKey,
    reverse: bool,
    search_all_folders: bool,
    conversations: bool,
    /// Folders with their own conversation setting, as Outlook's "This folder".
    conversation_folders: Vec<(accounts::FolderRef, bool)>,
    theme: looks::Theme,
    dark_messages: bool,
    /// The window as it was last (when not maximised), and whether it was maximised.
    window: Option<Place>,
    maximized: bool,
    folder_pane_width: f32,
    list_width: f32,
    /// One-row list: how wide the From column is (also in column_widths; kept for older builds).
    from_column: f32,
    /// One-row list: the columns that have been dragged to a width of their own.
    column_widths: Vec<(settings::ListCol, f32)>,
    list_height: f32,
}

/// Where the window was: top-left of the frame, and the inside size, in points.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
struct Place {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Place {
    /// Sensible enough to restore (not a stray value from a monitor that is gone).
    fn usable(&self) -> bool {
        self.w >= 600.0 && self.h >= 400.0 && self.w < 20000.0 && self.h < 20000.0 && self.x.abs() < 20000.0 && self.y.abs() < 20000.0
    }
}

impl Default for View {
    fn default() -> Self {
        View {
            tab: Tab::Home,
            ribbon_collapsed: false,
            ribbon_simplified: false,
            reading_pane: PaneAt::Right,
            folder_pane: true,
            sort: SortKey::Date,
            reverse: false,
            search_all_folders: true,
            conversations: false,
            conversation_folders: vec![],
            theme: looks::Theme::System,
            dark_messages: true,
            window: None,
            maximized: false,
            folder_pane_width: 230.0,
            list_width: 400.0,
            from_column: 220.0,
            column_widths: vec![],
            list_height: 300.0,
        }
    }
}

/// A line in the message list: a message (`child`: inside an opened conversation), or a
/// conversation's heading with its messages (indices into the list, newest first).
#[derive(Debug, Clone)]
enum Line {
    Msg {
        i: usize,
        child: bool,
    },
    Head {
        members: Vec<usize>,
        key: String,
        open: bool,
    },
    /// A date heading (Settings > Message list > Show date headers).
    Day(String),
}

/// The smaller dialogs the ribbon and menus open.
#[derive(Clone)]
enum Dialog {
    /// A new folder, inside `parent` when given.
    NewFolder {
        parent: Option<String>,
        name: String,
    },
    Rename {
        from: String,
        name: String,
    },
    DeleteFolder(String),
    Empty(String),
    RemoveAccount(usize),
    Source {
        title: String,
        text: String,
    },
    /// Sweep messages from one sender in one folder.
    Sweep {
        sender: String,
        folder: String,
        /// 0 all now, 1 all now and future, 2 keep only the latest, 3 keep the last `days`.
        choice: u8,
        days: u32,
        move_to: Option<String>,
    },
    /// Conversations on or off: for this folder or all of them.
    Conversations {
        on: bool,
    },
    Categorise,
    Categories(Vec<accounts::Category>),
    Rules,
    RuleEdit {
        index: Option<usize>,
        rule: mailcraft_core::rules::Rule,
    },
    About,
    Shortcuts,
}

/// The tabs under the ribbon, as Betterbird has them: the mail (always there), and Settings
/// and Account settings when open.
#[derive(Clone)]
pub enum MainTab {
    Mail,
    Settings {
        page: settings_ui::Page,
        search: String,
    },
    Accounts {
        page: settings_ui::Page,
        search: String,
    },
    Calendar,
    /// One message filling the window (double-click in the list), as Outlook's message window.
    Message {
        folder: String,
        uid: u32,
        subject: String,
    },
}

/// Whether two tabs are the same one (for keeping the active tab across a change to the list).
fn same_tab(a: &MainTab, b: &MainTab) -> bool {
    match (a, b) {
        (MainTab::Message { folder: f1, uid: u1, .. }, MainTab::Message { folder: f2, uid: u2, .. }) => f1 == f2 && u1 == u2,
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}

/// The new-mail card at the bottom right of the screen (Betterbird's alert).
struct Card {
    /// The line above the title ("New mail · address", "Reminder · in 15 minutes").
    heading: String,
    title: String,
    lines: Vec<String>,
    open: Option<(String, u32)>,
    /// A reminder: the day to show in the calendar.
    day: Option<chrono::NaiveDate>,
    /// When it appeared, or when the mouse last left it (it stays while the mouse is on it).
    born: std::time::Instant,
}

/// How long the card stays, and how long it takes to fade in and out.
const CARD_SECS: f32 = 7.0;
const CARD_FADE: f32 = 0.3;

/// A banner at the bottom left: new mail (click to open it), an error, or a confirmation.
struct Banner {
    title: String,
    lines: Vec<String>,
    open: Option<(String, u32)>,
    born: std::time::Instant,
    error: bool,
    /// Stays until dismissed.
    sticky: bool,
}

struct OpenMessage {
    folder: String,
    uid: u32,
    subject: String,
    from: String,
    to: String,
    date: String,
    attachments: Vec<(String, String, Vec<u8>)>,
    reader: WordApp,
    // For replying and forwarding.
    message_id: Option<String>,
    in_reply_to: Option<String>,
    references: Vec<String>,
    reply_to: Vec<String>,
    to_list: Vec<String>,
    cc_list: Vec<String>,
    html: String,
    /// Who to tell it was read, when the sender asked (Disposition-Notification-To).
    receipt_to: Option<String>,
    // For the browser view.
    inline: Vec<htmlview::Inline>,
    remote: bool,
    show_remote: bool,
    prepared: String,
    /// The rest of the conversation under it, as cards (Settings > Message list).
    thread: String,
    /// A meeting invitation (or cancellation, or reply) it carries.
    invite: Option<invite::Invite>,
}

/// A message popped out of the window into one of its own (drag its tab out, or the tab's
/// menu), to read beside another (25 Oct: "I can drag it onto another screen").
struct Popout {
    id: u64,
    /// Taken out only for a moment, to reply from it.
    msg: Option<OpenMessage>,
    title: String,
    /// Where it opened (screen points).
    at: Option<egui::Pos2>,
    #[cfg(any(windows, target_os = "linux"))]
    web: Option<htmlview::HtmlView>,
    /// Its native window, once found (each popped-out window has its own browser view).
    native: Option<u64>,
    /// Frames spent looking for that window; then the built-in renderer is used.
    tries: u32,
    web_failed: bool,
}

/// What a popped-out message's buttons ask for.
enum PopAct {
    Close,
    Dock,
    Reply(Action),
}

struct Compose {
    kind: &'static str,
    /// Sending from a send-only address (Settings > Email accounts) instead of the account's.
    from: Option<String>,
    to: String,
    cc: String,
    bcc: String,
    /// Bcc shown (the Bcc button, or always by setting).
    show_bcc: bool,
    /// The forgotten-attachment question is showing; once answered "Send anyway", not again.
    ask_attach: bool,
    attach_ok: bool,
    subject: String,
    editor: WordApp,
    in_reply_to: Option<String>,
    references: Vec<String>,
    replied: Option<(String, u32)>,
    attachments: Vec<(String, String, Vec<u8>)>,
    sending: bool,
    /// Ctrl+Enter was pressed: send on the next frame.
    send_now: bool,
    error: Option<String>,
    // Drafts: the Message-ID every save of this message carries, whether a copy is on the
    // server, the fingerprint of what was last saved, and when autosave last looked.
    draft_id: String,
    on_server: bool,
    saved: u64,
    saving: Option<u64>,
    checked: std::time::Instant,
    note: Option<String>,
}

impl Compose {
    fn blank(kind: &'static str, editor: WordApp, email: &str) -> Compose {
        let mut c = Compose {
            kind,
            from: None,
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            show_bcc: false,
            ask_attach: false,
            attach_ok: false,
            subject: String::new(),
            editor,
            in_reply_to: None,
            references: vec![],
            replied: None,
            attachments: vec![],
            sending: false,
            send_now: false,
            error: None,
            draft_id: new_message_id(email),
            on_server: false,
            saved: 0,
            saving: None,
            checked: std::time::Instant::now(),
            note: None,
        };
        c.saved = c.fingerprint();
        c
    }

    /// What the message is now, so autosave only saves when something changed.
    fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (&self.to, &self.cc, &self.bcc, &self.subject).hash(&mut h);
        wordcraft_formats::html::export(&self.editor.session.doc).hash(&mut h);
        for (name, _, bytes) in &self.attachments {
            (name, bytes.len()).hash(&mut h);
        }
        h.finish()
    }

    /// The message to send: `name` is the sender's display name; `plain` sends text only.
    fn outgoing(&self, acct: &Account, name: Option<String>, plain: bool) -> mailcraft_core::send::Outgoing {
        let doc = &self.editor.session.doc;
        mailcraft_core::send::Outgoing {
            from: acct.email.clone(),
            from_name: name,
            to: mailcraft_core::send::split_addresses(&self.to),
            cc: mailcraft_core::send::split_addresses(&self.cc),
            bcc: mailcraft_core::send::split_addresses(&self.bcc),
            subject: self.subject.clone(),
            html: if plain { String::new() } else { wordcraft_formats::html::export(doc) },
            text: String::from_utf8_lossy(&wordcraft_formats::txt::export(doc)).to_string(),
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
            attachments: self.attachments.clone(),
            message_id: None,
            auto_reply: false,
            reply_to: Some(acct.reply_to.clone()).filter(|r| !r.trim().is_empty()),
            organisation: Some(acct.organisation.clone()).filter(|o| !o.trim().is_empty()),
            calendar: None,
        }
    }

    /// The saved draft to remove once this is sent or discarded (a save still on its way counts).
    fn draft_on_server(&self) -> Option<String> {
        (self.on_server || self.saving.is_some()).then(|| self.draft_id.clone())
    }
}

/// A fresh Message-ID on the account's own domain.
fn new_message_id(email: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    let domain = email.rsplit_once('@').map(|(_, d)| d).filter(|d| !d.is_empty()).unwrap_or("mailcraft.local");
    format!("{t:x}.{:x}.{}@{domain}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

/// How long autosave waits between looks.
const AUTOSAVE: std::time::Duration = std::time::Duration::from_secs(30);

/// A message's own words: what it quotes from earlier messages taken off.
fn without_quote(text: &str) -> String {
    let mut out: Vec<&str> = vec![];
    for line in text.lines() {
        let l = line.trim();
        if (l.starts_with("On ") && l.ends_with("wrote:")) || l.starts_with("-----Original Message") || l.starts_with("From: ") && out.len() > 2 {
            break;
        }
        if !l.starts_with('>') {
            out.push(line.trim_end());
        }
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    let s = out.join("\n");
    if s.chars().count() > 6000 { format!("{}...", s.chars().take(6000).collect::<String>()) } else { s }
}

/// The message as the reading pane shows it: recoloured for dark reading when that is on.
fn reading_html(html: &str, dark: bool) -> String {
    // Every link labelled with where it goes, shown on hover (links.rs).
    let html = links::label(html);
    if dark { darkmail::rewrite(&html) } else { html }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A WordCraft editor for writing mail: Web Layout, spelling on, no rulers. `room` blank
/// paragraphs go above any quoted text, with the caret on the first, ready to type.
fn editor(html: &str, room: usize) -> WordApp {
    let mut doc = wordcraft_formats::html::import(html);
    doc.ensure_nonempty();
    let mut session = Session::new(doc);
    session.view.mode = wordcraft_layout::ViewMode::Web;
    session.view.ruler = false;
    session.view.proofing = true;
    let mut app = WordApp::new(session, Services::default());
    // MailCraft's theme rules the window; the editor must not put Word's on it.
    app.manage_theme = false;
    app.ui.ribbon_collapsed = true;
    let _ = app.run("caret.docStart", serde_json::json!({}));
    for _ in 0..room {
        let _ = app.run("text.newParagraph", serde_json::json!({}));
    }
    let _ = app.run("caret.docStart", serde_json::json!({}));
    app.session.dirty = false;
    app
}

/// Files to attach: a picker (several at once), or for automated tests the paths in
/// MAILCRAFT_TEST_PICK (separated by ';').
fn pick_files() -> Vec<std::path::PathBuf> {
    if let Ok(list) = std::env::var("MAILCRAFT_TEST_PICK") {
        return list.split(';').filter(|p| !p.is_empty()).map(std::path::PathBuf::from).collect();
    }
    rfd::FileDialog::new().set_title("Attach files").pick_files().unwrap_or_default()
}

/// Read a file as an attachment: (name, MIME type, bytes).
fn read_attachment(path: &std::path::Path) -> Result<(String, String, Vec<u8>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "attachment".into());
    let mime = mime_guess::from_path(path).first_or_octet_stream().essence_str().to_string();
    Ok((name, mime, bytes))
}

/// Save an attachment where the reader chooses.
fn save_attachment(name: &str, bytes: &[u8]) -> Result<Option<String>, String> {
    let Some(path) = rfd::FileDialog::new().set_title("Save attachment").set_file_name(name).save_file() else { return Ok(None) };
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(Some(path.display().to_string()))
}

/// Open an attachment with its usual program, from a copy in the temp folder.
fn open_attachment(name: &str, bytes: &[u8]) -> Result<(), String> {
    let dir = std::env::temp_dir().join("MailCraft");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Keep only the file name: a hostile name must not climb out of the folder.
    let safe: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    let path = dir.join(if safe.trim().is_empty() { "attachment".into() } else { safe });
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    return open::that_detached(&path).map_err(|e| e.to_string());
    #[cfg(not(windows))]
    Err(format!("saved to {} (opening files is Windows-only for now)", path.display()))
}

fn with_prefix(prefix: &str, subject: &str) -> String {
    let lower = subject.to_ascii_lowercase();
    if lower.starts_with(&prefix.to_ascii_lowercase()) { subject.to_string() } else { format!("{prefix} {subject}") }
}

struct MailApp {
    /// The calendar the last accepted invitation went in.
    invite_last: Option<i64>,
    accounts: Vec<Account>,
    store: Option<Store>,
    worker: Worker,
    account: usize,
    folders: Vec<FolderRow>,
    folder: Option<String>,
    messages: Vec<MessageRow>,
    /// The selected message: (folder, uid). Lists can span folders.
    selected: Option<(String, u32)>,
    open: Option<OpenMessage>,
    loading: Option<u32>,
    /// The message asked for and when, for the log's "on screen in" time.
    open_asked: Option<((String, u32), std::time::Instant)>,
    status: String,
    add: Option<AddDialog>,
    prompt: Option<(usize, String)>,
    compose: Option<Compose>,
    #[cfg(any(windows, target_os = "linux"))]
    web: Option<htmlview::HtmlView>,
    web_error: Option<String>,
    syncing: bool,
    styled: u32,
    /// The address whose browser sign-in is under way.
    signing_in: Option<String>,
    prompt_error: Option<String>,
    reg_id: String,
    reg_secret: String,
    /// Words in the search box, and the server's answer for (folder, words) once it came.
    search: String,
    /// The server's answer for (folder, or None for all of them; words): hits per folder.
    server_hits: Option<(Option<String>, String, Vec<(String, Vec<u32>)>)>,
    /// Search every folder of the account, not just the open one.
    search_all: bool,
    searching: bool,
    /// How far the server search has got: (folders done, folders in all).
    search_progress: Option<(usize, usize)>,
    /// Mail being downloaded for offline reading and search: (account, here, in all).
    offline_progress: Option<(String, i64, i64)>,
    /// Messages in the search's scope not downloaded yet: (scope, when counted, how many).
    missing_count: Option<(Option<String>, std::time::Instant, i64)>,
    /// The messages the Move dialog is moving.
    /// The messages the Move dialog is moving (folder, UIDs, and true to copy instead).
    moving: Option<(Vec<(String, u32)>, bool)>,
    /// Several messages picked with Ctrl/Shift+click or Ctrl+A (empty or one: just `selected`).
    picked: Vec<(String, u32)>,
    /// The list as shown (conversation view groups it).
    lines: Vec<Line>,
    /// A list line the keyboard moved to, to scroll into view on the next frame.
    list_reveal: Option<usize>,
    /// The list's scroll offset and visible height last frame, and its plain row height
    /// (Page Up/Down move by what fits; 31 Oct 2026).
    list_view: (f32, f32, f32),
    /// Conversation view for every folder, unless the folder has its own setting.
    threads: bool,
    conv_folders: Vec<(accounts::FolderRef, bool)>,
    /// Sent messages shown inside conversations (not part of the folder; indices after `messages`).
    extra: Vec<MessageRow>,
    expanded: std::collections::HashSet<String>,
    /// Addresses written to from this account (its Sent folder).
    known: std::collections::HashSet<String>,
    /// Key tips over the ribbon; Alt is down with nothing else pressed yet (a tap shows them).
    keytips: ribbon::KeyTips,
    alt_clean: bool,
    /// The new-mail card, its icon, and a message it was clicked to open.
    card: Option<Card>,
    card_icon: Option<egui::TextureHandle>,
    /// Messages popped out into windows of their own; one asked for before it had loaded; the
    /// number the next one gets; a message tab being dragged (its place and where the pointer was).
    popouts: Vec<Popout>,
    pop_pending: Option<(String, u32, Option<egui::Pos2>)>,
    pop_seq: u64,
    tab_drag: Option<(usize, egui::Pos2)>,
    card_open: Option<(String, u32)>,
    card_day: Option<chrono::NaiveDate>,
    /// File > Export settings / Import settings, while open.
    transfer: Option<transfer::Form>,
    /// The Calendar tab and the calendar accounts.
    cal: calendar::CalState,
    /// Banners showing (Settings > Notifications, Accessibility).
    banners: Vec<Banner>,
    /// The Inbox's highest UID before a sync: (address, UID); what is above it after is new.
    /// Per account, where each folder ended before the sync now running: what it brings is new.
    new_mail_marks: std::collections::HashMap<String, std::collections::HashMap<String, u32>>,
    /// Send-only addresses (senders.json), and the Settings form adding or changing one.
    senders: Vec<accounts::Sender>,
    sender_form: Option<settings_ui::SenderForm>,
    /// An account being edited in Settings.
    account_form: Option<settings_ui::AccountForm>,
    /// The window title as last sent, and when MailCraft started.
    last_title: String,
    started: std::time::Instant,
    /// The tabs under the ribbon, and which is showing.
    tabs: Vec<MainTab>,
    active_tab: usize,
    /// The account picked in the Account settings tab.
    settings_acct: usize,
    /// Who to suggest in To, Cc and Bcc.
    people: Vec<people::Person>,
    /// A Settings button waiting for its second click (emptying a folder).
    set_confirm: Option<String>,
    /// Settings > Privacy and data is listing the hidden contacts.
    set_show_hidden: bool,
    /// Storage in use on the server (Settings > Storage): (address, KB used and limit).
    quota: Option<(String, Result<Option<(u64, u64)>, String>)>,
    /// The folder Settings > Search last set "All folders" for.
    scope_for: Option<String>,
    /// The Focused Inbox is showing Other; unread messages there.
    show_other: bool,
    other_unread: usize,
    /// Text size and animations as last put on the window.
    zoom_applied: Option<f32>,
    animations_applied: Option<bool>,
    /// The conversation whose heading was clicked: actions take all of it.
    head: Option<Vec<(String, u32)>>,
    theme: looks::Theme,
    /// The theme in effect (System settled), as last applied to the window.
    applied: Option<looks::Theme>,
    /// In the dark theme, show mail dark too (Outlook's default), or as sent.
    dark_messages: bool,
    rules: Vec<mailcraft_core::rules::Rule>,
    settings: settings::Settings,
    /// The item picked in a Settings list (templates, quick steps...).
    set_sel: Option<usize>,
    /// The search folder being shown instead of a folder (index into settings.search_folders).
    vfolder: Option<usize>,
    /// The folders shown, for Back and Forward (Alt+Left / Alt+Right, the mouse's side buttons).
    history: history::History,
    /// Deletes that Ctrl+Z can take back, newest last.
    undo: Vec<Undo>,
    /// An undo waiting for the delete to reach Trash (the server is still moving it): since
    /// when, and when this PC's copy was last looked at for it.
    undo_wait: Option<(Undo, std::time::Instant, std::time::Instant)>,
    /// Folders being refreshed (F5): (address, folder).
    refreshing: std::collections::HashSet<(String, String)>,
    /// What is being typed into the blocked / safe senders lists.
    new_blocked: String,
    new_safe: String,
    /// When every account was last checked for new mail.
    last_check: std::time::Instant,
    /// A message to mark read once it has been open long enough.
    read_timer: Option<((String, u32), std::time::Instant)>,
    /// New mail arrived: flash the taskbar (when MailCraft is not in front).
    alert: bool,
    /// The window is minimised: eframe draws nothing, so cards go to Windows (notify.rs).
    hidden: bool,
    /// Clicks on those Windows notifications.
    notified: (std::sync::mpsc::Sender<notify::Clicked>, std::sync::mpsc::Receiver<notify::Clicked>),
    /// Email links from later launches (mailto.rs), and ones waiting for the window to be ready.
    links: Option<std::sync::mpsc::Receiver<mailto::Message>>,
    pending_links: Vec<String>,
    /// Told the user a link is waiting for the message they are writing.
    link_waiting: bool,
    /// The open message asked for a read receipt: (to whom, its subject, its date).
    receipt: Option<(String, String, String)>,
    /// Closing, waiting for Trash to be emptied first: (empties still going, since).
    exiting: Option<(usize, std::time::Instant)>,
    /// Trash emptied (or not wanted): the window may close.
    allow_close: bool,
    categories: Vec<accounts::Category>,
    folder_prefs: accounts::FolderPrefs,
    /// The favourite folders as shown: each with its row from the cache, when there is one.
    fav_rows: Vec<(accounts::FolderRef, Option<FolderRow>)>,
    /// Leave the status bar alone at the next sync (it reports an action just done).
    keep_status: bool,
    /// Whether Ctrl was held during the drag so far (see the drop).
    drag_copy: bool,
    tab: Tab,
    fold: ribbon::Fold,
    /// The view settings last saved, to save again only when they change.
    saved_view: Option<View>,
    /// When the view file was last written (moving the window changes it every frame).
    view_written: std::time::Instant,
    place: Option<Place>,
    maximized: bool,
    folder_w: f32,
    list_w: f32,
    /// One-row list: the From column's width, the widths of columns that have been dragged
    /// (the others keep their own), and a column being dragged to a new place.
    from_col: f32,
    col_w: std::collections::HashMap<settings::ListCol, f32>,
    col_drag: Option<settings::ListCol>,
    list_h: f32,
    pane_at: PaneAt,
    show_folders: bool,
    sort: SortKey,
    /// Flip the natural order (newest first; A to Z; largest first).
    reverse: bool,
    filter: Filter,
    dialog: Option<Dialog>,
    focus_search: bool,
    /// Collapsed folders (full names) and collapsed accounts (addresses).
    collapsed: std::collections::HashSet<String>,
}

/// A stored UTC date in local time: just the time for today, else the date.
/// How dates are written in the list (General > Date and time): 0 day first, 1 month first,
/// 2 ISO; and the 24-hour clock.
static DATE_STYLE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static CLOCK_24: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

fn set_date_style(s: &settings::Settings) {
    use std::sync::atomic::Ordering;
    let v = match s.date_style {
        settings::DateStyle::DayMonth => 0,
        settings::DateStyle::MonthDay => 1,
        settings::DateStyle::Iso => 2,
    };
    DATE_STYLE.store(v, Ordering::Relaxed);
    CLOCK_24.store(s.time_24h, Ordering::Relaxed);
}

/// Windows' "Animation effects" (Settings > Accessibility > Visual effects), read once.
fn system_animations() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        #[cfg(windows)]
        {
            // The switch writes MinAnimate "0" when animations are off.
            windows_registry::CURRENT_USER
                .open("Control Panel\\Desktop\\WindowMetrics")
                .and_then(|k| k.get_string("MinAnimate"))
                .map(|v| v.trim() != "0")
                .unwrap_or(true)
        }
        #[cfg(not(windows))]
        {
            true
        }
    })
}

/// The date heading a message goes under, as Outlook groups its list: Today, Yesterday, the
/// days of this week by name, Last week, Earlier this month, then month by month.
fn day_label(d: &Option<String>, today: chrono::NaiveDate) -> String {
    use chrono::Datelike;
    let Some(day) = d.as_deref().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|t| t.with_timezone(&chrono::Local).date_naive())
    else {
        return "Older".into();
    };
    let ago = (today - day).num_days();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    if ago <= 0 {
        "Today".into()
    } else if ago == 1 {
        "Yesterday".into()
    } else if day >= monday {
        day.format("%A").to_string()
    } else if day >= monday - chrono::Duration::days(7) {
        "Last week".into()
    } else if day.year() == today.year() && day.month() == today.month() {
        "Earlier this month".into()
    } else {
        day.format("%B %Y").to_string()
    }
}

/// Up to two initials for a sender picture ("Sam Taylor" ST, "news@shop.example" N).
fn initials(name: &str) -> String {
    let name = name.split('@').next().unwrap_or(name);
    let words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let first = |w: &str| w.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    match (words.first(), words.get(1).filter(|_| !name.contains('.') || name.contains(' '))) {
        (Some(a), Some(b)) => format!("{}{}", first(a), first(b)),
        (Some(a), None) => first(a),
        _ => "?".into(),
    }
}

/// A steady colour per sender for their picture.
fn sender_colour(key: &str) -> egui::Color32 {
    const SET: [(u8, u8, u8); 10] = [
        (0x0F, 0x6C, 0xBD),
        (0xC2, 0x39, 0xB3),
        (0x00, 0x7A, 0x6E),
        (0xCA, 0x50, 0x10),
        (0x5C, 0x2E, 0x91),
        (0x49, 0x82, 0x05),
        (0xA4, 0x26, 0x2C),
        (0x03, 0x83, 0x87),
        (0x8E, 0x56, 0x2E),
        (0x40, 0x4C, 0x9C),
    ];
    let h = key.to_ascii_lowercase().bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    let (r, g, b) = SET.get(h as usize % SET.len()).copied().unwrap_or((0x0F, 0x6C, 0xBD));
    egui::Color32::from_rgb(r, g, b)
}

/// One column of the one-row list: which (None for Subject) and where.
struct Cell {
    col: Option<settings::ListCol>,
    x0: f32,
    x1: f32,
}

/// The one-row list's columns in the order they are kept in (Settings; they can be dragged
/// about); Subject takes the room the others leave. `widths` has every other column's width.
fn list_cells(left: f32, right: f32, cols: &[settings::ListCol], widths: &std::collections::HashMap<settings::ListCol, f32>) -> Vec<Cell> {
    use settings::ListCol as C;
    let mut order: Vec<C> = cols.to_vec();
    if !order.contains(&C::Subject) {
        order.push(C::Subject);
    }
    let start = left + 6.0;
    // Room at the end for the column picker.
    let end = right - 28.0;
    let w = |c: &C| widths.get(c).copied().unwrap_or_else(|| c.width(220.0));
    let fixed: f32 = order.iter().filter(|c| **c != C::Subject).map(w).sum();
    let subject = (end - start - fixed).max(120.0);
    let mut x = start;
    order
        .into_iter()
        .map(|c| {
            let w = if c == C::Subject { subject } else { w(&c) };
            let cell = Cell { col: (c != C::Subject).then_some(c), x0: x, x1: x + w };
            x += w;
            cell
        })
        .collect()
}

/// Columns whose text sits at the right of the cell.
fn right_aligned(c: Option<settings::ListCol>) -> bool {
    use settings::ListCol as C;
    matches!(c, Some(C::Date | C::Received | C::Size | C::Unread | C::Total | C::Order))
}

/// What a message's Status column says.
fn status_text(m: &MessageRow) -> &'static str {
    let has = |f: &str| m.flags.split_whitespace().any(|x| x.eq_ignore_ascii_case(f));
    if has("\\Answered") {
        "Replied"
    } else if has("$Forwarded") {
        "Forwarded"
    } else if !m.seen() {
        "New"
    } else {
        "Read"
    }
}

/// A conversation symbol (two speech marks), for the Thread column's heading.
fn draw_thread(p: &egui::Painter, c: egui::Pos2, col: egui::Color32) {
    let s = egui::Stroke::new(1.2, col);
    p.rect_stroke(egui::Rect::from_center_size(c + vec2(-2.0, -2.0), vec2(9.0, 7.0)), 2.0, s, egui::StrokeKind::Middle);
    p.rect_stroke(egui::Rect::from_center_size(c + vec2(2.5, 3.0), vec2(9.0, 7.0)), 2.0, s, egui::StrokeKind::Middle);
}

/// A size as the list shows it.
fn size_text(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    if b >= 1024.0 * 1024.0 { format!("{:.1} MB", b / 1024.0 / 1024.0) } else { format!("{} KB", (b / 1024.0).ceil().max(1.0) as i64) }
}

/// A tick box, 12 x 12, centred on `c`.
fn tick_box(p: &egui::Painter, c: egui::Pos2, on: bool, t: &Tokens) {
    let r = egui::Rect::from_center_size(c, vec2(12.0, 12.0));
    if on {
        p.rect_filled(r, 2.0, t.accent);
        let s = egui::Stroke::new(1.6, egui::Color32::WHITE);
        p.add(egui::Shape::line(vec![c + vec2(-3.5, 0.0), c + vec2(-1.0, 2.8), c + vec2(3.8, -2.8)], s));
    } else {
        p.rect_stroke(r, 2.0, egui::Stroke::new(1.0, t.text_dim), egui::StrokeKind::Inside);
    }
}

/// A paper clip, about 10 x 14, at `at` (top-left).
fn draw_clip(p: &egui::Painter, at: egui::Pos2, c: egui::Color32) {
    let s = egui::Stroke::new(1.2, c);
    let pt = |x: f32, y: f32| at + vec2(x, y);
    p.add(egui::Shape::line(
        vec![pt(7.0, 4.0), pt(7.0, 11.0), pt(5.5, 13.0), pt(4.0, 11.0), pt(4.0, 2.5), pt(6.0, 0.5), pt(8.5, 0.5), pt(10.0, 2.5), pt(10.0, 11.5)],
        s,
    ));
}

fn short_date(d: &Option<String>) -> String {
    use std::sync::atomic::Ordering;
    let Some(s) = d.as_deref() else { return String::new() };
    match chrono::DateTime::parse_from_rfc3339(s) {
        Ok(t) => {
            let local = t.with_timezone(&chrono::Local);
            let time = if CLOCK_24.load(Ordering::Relaxed) { "%H:%M" } else { "%-I:%M %p" };
            if local.date_naive() == chrono::Local::now().date_naive() {
                local.format(time).to_string()
            } else {
                let date = match DATE_STYLE.load(Ordering::Relaxed) {
                    1 => "%m/%d/%Y",
                    2 => "%Y-%m-%d",
                    _ => "%d/%m/%Y",
                };
                local.format(&format!("{date} {time}")).to_string()
            }
        }
        Err(_) => s.get(..16).unwrap_or(s).replace('T', " "),
    }
}

impl MailApp {
    fn new(ctx: &egui::Context, first_link: Option<String>, single: bool) -> Self {
        let accounts = accounts::load();
        let store = Store::open(accounts::cache_path()).ok();
        let mut app = MailApp {
            invite_last: None,
            worker: worker::start(ctx.clone()),
            store,
            account: 0,
            folders: vec![],
            folder: None,
            messages: vec![],
            selected: None,
            open: None,
            loading: None,
            open_asked: None,
            status: String::new(),
            add: None,
            prompt: None,
            compose: None,
            #[cfg(any(windows, target_os = "linux"))]
            web: None,
            web_error: None,
            syncing: false,
            styled: 0,
            signing_in: None,
            prompt_error: None,
            reg_id: String::new(),
            reg_secret: String::new(),
            search: String::new(),
            server_hits: None,
            search_all: true,
            searching: false,
            search_progress: None,
            offline_progress: None,
            missing_count: None,
            moving: None,
            picked: vec![],
            list_reveal: None,
            list_view: (0.0, 0.0, 0.0),
            lines: vec![],
            threads: false,
            conv_folders: vec![],
            extra: vec![],
            expanded: Default::default(),
            known: Default::default(),
            people: vec![],
            senders: accounts::load_senders(),
            sender_form: None,
            account_form: None,
            last_title: String::new(),
            started: std::time::Instant::now(),
            tabs: vec![MainTab::Mail],
            active_tab: 0,
            settings_acct: 0,
            banners: vec![],
            card: None,
            card_icon: None,
            popouts: vec![],
            pop_pending: None,
            pop_seq: 0,
            tab_drag: None,
            card_open: None,
            card_day: None,
            transfer: None,
            cal: calendar::CalState::new(),
            keytips: Default::default(),
            alt_clean: false,
            new_mail_marks: std::collections::HashMap::new(),
            quota: None,
            set_confirm: None,
            set_show_hidden: false,
            scope_for: None,
            show_other: false,
            other_unread: 0,
            zoom_applied: None,
            animations_applied: None,
            head: None,
            theme: looks::Theme::System,
            applied: None,
            dark_messages: true,
            rules: accounts::load_rules(),
            settings: settings::load(),
            set_sel: None,
            vfolder: None,
            history: history::History::default(),
            undo: vec![],
            undo_wait: None,
            refreshing: std::collections::HashSet::new(),
            new_blocked: String::new(),
            new_safe: String::new(),
            last_check: std::time::Instant::now(),
            read_timer: None,
            alert: false,
            hidden: false,
            notified: std::sync::mpsc::channel(),
            // A --new-window copy leaves later launches to the window that was open first.
            links: if single { mailto::listen(ctx.clone()) } else { None },
            pending_links: first_link.into_iter().collect(),
            link_waiting: false,
            receipt: None,
            exiting: None,
            allow_close: false,
            categories: accounts::load_categories(),
            folder_prefs: accounts::load_folder_prefs(),
            fav_rows: vec![],
            keep_status: false,
            drag_copy: false,
            tab: Tab::Home,
            fold: ribbon::Fold::default(),
            saved_view: None,
            view_written: std::time::Instant::now(),
            place: None,
            maximized: false,
            folder_w: 230.0,
            list_w: 400.0,
            from_col: 220.0,
            col_w: Default::default(),
            col_drag: None,
            list_h: 300.0,
            pane_at: PaneAt::Right,
            show_folders: true,
            sort: SortKey::Date,
            reverse: false,
            filter: Filter::All,
            dialog: None,
            focus_search: false,
            collapsed: Default::default(),
            accounts,
        };
        app.status = if app.store.is_none() {
            "The local mail cache could not be opened.".into()
        } else if app.accounts.is_empty() {
            "Add an account to start.".into()
        } else {
            "Ready.".into()
        };
        let v: View = accounts::read_settings("view.json").unwrap_or_default();
        app.tab = if v.tab == Tab::File { Tab::Home } else { v.tab };
        app.fold.collapsed = v.ribbon_collapsed;
        app.fold.simplified = v.ribbon_simplified;
        app.pane_at = v.reading_pane;
        app.show_folders = v.folder_pane;
        app.sort = v.sort;
        app.reverse = v.reverse;
        app.search_all = v.search_all_folders;
        app.threads = v.conversations;
        app.conv_folders = v.conversation_folders.clone();
        app.theme = v.theme;
        app.dark_messages = v.dark_messages;
        app.place = v.window;
        app.maximized = v.maximized;
        app.folder_w = v.folder_pane_width.clamp(120.0, 800.0);
        app.list_w = v.list_width.clamp(200.0, 1600.0);
        app.from_col = v.from_column.clamp(100.0, 500.0);
        app.col_w = v.column_widths.iter().map(|&(c, w)| (c, w.clamp(c.min_width(), 1200.0))).collect();
        app.col_w.entry(settings::ListCol::From).or_insert(app.from_col);
        app.list_h = v.list_height.clamp(120.0, 1200.0);
        app.saved_view = Some(v);
        set_date_style(&app.settings);
        app.refresh_folders();
        if !app.accounts.is_empty() {
            app.sync(0);
        }
        app
    }

    fn current(&self) -> Option<&Account> {
        self.accounts.get(self.account)
    }

    fn refresh_favourites(&mut self) {
        let Some(store) = &self.store else { return };
        self.fav_rows = self
            .folder_prefs
            .favourites
            .iter()
            .filter(|r| self.accounts.iter().any(|a| a.email == r.account))
            .map(|r| (r.clone(), store.folders(&r.account).ok().and_then(|fs| fs.into_iter().find(|f| f.name == r.folder))))
            .collect();
    }

    fn save_folder_prefs(&mut self) {
        if let Err(e) = accounts::save_folder_prefs(&self.folder_prefs) {
            self.status = format!("Could not save folder settings: {e}");
        }
        self.refresh_favourites();
    }

    /// The categories a message has: kept here, or keywords on the server that are categories.
    fn row_categories(&self, m: &MessageRow) -> Vec<&accounts::Category> {
        let keys: Vec<&str> = m.categories.split_whitespace().chain(m.flags.split_whitespace().filter(|f| !f.starts_with('\\'))).collect();
        self.categories.iter().filter(|c| keys.iter().any(|k| k.eq_ignore_ascii_case(&c.key))).collect()
    }

    fn refresh_folders(&mut self) {
        self.refresh_favourites();
        let (Some(store), Some(acct)) = (&self.store, self.accounts.get(self.account)) else { return };
        self.folders = store.folders(&acct.email).unwrap_or_default();
        if self.folder.is_none() {
            self.folder = self.folders.first().map(|f| f.name.clone());
        }
        self.refresh_known();
        self.refresh_messages();
    }

    fn refresh_messages(&mut self) {
        self.load_messages();
        self.arrange();
    }

    /// Apply the View tab's filter and sort to the list.
    fn arrange(&mut self) {
        match self.filter {
            Filter::All => {}
            Filter::Unread => self.messages.retain(|m| !m.seen()),
            Filter::Flagged => self.messages.retain(flagged),
        }
        if self.focused_here() {
            self.other_unread = self.messages.iter().filter(|m| !m.seen() && self.is_other(m)).count();
            let keep: Vec<bool> = self.messages.iter().map(|m| self.is_other(m) == self.show_other).collect();
            let mut k = keep.into_iter();
            self.messages.retain(|_| k.next().unwrap_or(true));
        }
        let lower = |s: &Option<String>| s.as_deref().unwrap_or("").to_lowercase();
        match self.sort {
            SortKey::Date => self.messages.sort_by(|a, b| b.date.cmp(&a.date).then(b.uid.cmp(&a.uid))),
            SortKey::From => self.messages.sort_by_key(|m| lower(&m.from_name.clone().or(m.from_addr.clone()))),
            SortKey::Subject => self.messages.sort_by_key(|m| bare_subject(m.subject.as_deref().unwrap_or(""))),
            SortKey::Size => self.messages.sort_by_key(|m| std::cmp::Reverse(m.size)),
        }
        if self.reverse {
            self.messages.reverse();
        }
        self.build_lines();
    }

    /// The list's lines: one per message, or (conversation view) one per conversation, with
    /// its messages under it when opened.
    fn build_lines(&mut self) {
        self.build_message_lines();
        // (Search results used to start with "Top results", the best matches by a guess at
        // relevance; taken out 2026-10-25: "not once have I seen relevant emails there".)
        // Date headings over the lines when the list is in date order.
        if self.settings.date_headers && self.sort == SortKey::Date {
            let today = chrono::Local::now().date_naive();
            let lines = std::mem::take(&mut self.lines);
            let mut last = String::new();
            for l in lines {
                let top = match &l {
                    Line::Msg { child: false, i } => self.row(*i),
                    Line::Head { members, .. } => members.first().and_then(|&i| self.row(i)),
                    _ => None,
                };
                if let Some(m) = top {
                    let label = day_label(&m.date, today);
                    if label != last {
                        self.lines.push(Line::Day(label.clone()));
                        last = label;
                    }
                }
                self.lines.push(l);
            }
        }
    }

    fn build_message_lines(&mut self) {
        self.lines.clear();
        self.extra.clear();
        if !self.threads_on() {
            self.lines = (0..self.messages.len()).map(|i| Line::Msg { i, child: false }).collect();
            return;
        }
        // Your replies are in Sent: bring them into the conversations they belong to; and,
        // unless Settings says not to, what was deleted from them (Trash).
        let trash = self.folders.iter().find(|f| foldertree::kind(f) == foldertree::Kind::Trash).map(|f| f.name.clone());
        let mut also = vec![self.sent_name()];
        if self.settings.conversation_deleted {
            also.push(trash);
        }
        for other in also.into_iter().flatten() {
            let (Some(store), Some(acct)) = (&self.store, self.current()) else { break };
            if self.folder.as_deref() == Some(other.as_str()) {
                continue;
            }
            let Ok(Some(id)) = store.folder_id(&acct.email, &other) else { continue };
            let have_ids: std::collections::HashSet<String> = self
                .messages
                .iter()
                .chain(self.extra.iter())
                .filter_map(|m| m.message_id.as_deref())
                .map(|i| i.trim().to_ascii_lowercase())
                .collect();
            let more: Vec<MessageRow> = store
                .messages(id, 3000)
                .unwrap_or_default()
                .into_iter()
                .filter(|m| !self.messages.iter().any(|x| x.uid == m.uid && x.folder == m.folder))
                // One you sent yourself is in both: show it once.
                .filter(|m| m.message_id.as_deref().is_none_or(|i| !have_ids.contains(&i.trim().to_ascii_lowercase())))
                .collect();
            self.extra.extend(more);
        }
        let n = self.messages.len();
        let mut all: Vec<MessageRow> = Vec::with_capacity(n + self.extra.len());
        all.extend(self.messages.iter().cloned());
        all.extend(self.extra.iter().cloned());
        for mut members in mailcraft_core::threads::group(&all) {
            // Only conversations with something in this folder; a Sent message alone is not shown.
            if !members.iter().any(|&i| i < n) {
                continue;
            }
            if members.len() == 1 {
                if let Some(&i) = members.first() {
                    self.lines.push(Line::Msg { i, child: false });
                }
                continue;
            }
            // Newest first, Sent messages included.
            members.sort_by(|a, b| {
                let (da, db) = (all.get(*a).and_then(|m| m.date.clone()), all.get(*b).and_then(|m| m.date.clone()));
                db.cmp(&da).then(a.cmp(b))
            });
            // Keyed by its oldest message, which stays the same as new replies arrive.
            let key = members.last().and_then(|&i| self.row(i)).map(|m| format!("{}/{}", m.folder, m.uid)).unwrap_or_default();
            let open = self.expanded.contains(&key);
            self.lines.push(Line::Head { members: members.clone(), key, open });
            if open {
                for &i in &members {
                    self.lines.push(Line::Msg { i, child: true });
                }
            }
        }
    }

    fn load_messages(&mut self) {
        let (Some(store), Some(acct)) = (&self.store, self.accounts.get(self.account)) else { return };
        // A search folder: its saved search over the whole account (not Trash or Junk).
        if let Some(sf) = self.vfolder.and_then(|i| self.settings.search_folders.get(i)).cloned() {
            let mut rows = if sf.words.trim().is_empty() {
                store.messages_account(&acct.email, 3000)
            } else {
                store.search_account(&acct.email, &sf.words, 3000)
            }
            .unwrap_or_default();
            rows.retain(|m| {
                !matches!(self.kind_of(&m.folder), foldertree::Kind::Trash | foldertree::Kind::Junk)
                    && (!sf.unread_only || !m.seen())
                    && (!sf.flagged_only || flagged(m))
                    && sf
                        .category
                        .as_ref()
                        .is_none_or(|k| m.categories.split_whitespace().chain(m.flags.split_whitespace()).any(|x| x.eq_ignore_ascii_case(k)))
            });
            self.messages = rows;
            return;
        }
        let words = self.search.trim().to_string();
        if !words.is_empty() && self.search_all {
            let mut found = store.search_account(&acct.email, &words, 2000).unwrap_or_default();
            if let Some((None, w, hits)) = &self.server_hits
                && *w == words
            {
                for (f, uids) in hits {
                    if let Ok(Some(id)) = store.folder_id(&acct.email, f) {
                        found.extend(store.by_uids(id, uids).unwrap_or_default());
                    }
                }
            }
            // Junk never; Trash when Settings > Search says so.
            let deleted = self.settings.search_deleted;
            found.retain(|m| match self.kind_of(&m.folder) {
                foldertree::Kind::Junk => false,
                foldertree::Kind::Trash => deleted,
                _ => true,
            });
            self.messages = tidy_results(found, &self.folders);
            return;
        }
        let Some(folder) = &self.folder else { return };
        if let Ok(Some(id)) = store.folder_id(&acct.email, folder) {
            self.messages = if words.is_empty() {
                store.messages(id, 2000).unwrap_or_default()
            } else {
                let mut found = store.search(id, &words, 2000).unwrap_or_default();
                // Add what the server's full-text search found that the cache could not see.
                if let Some((Some(f), w, hits)) = &self.server_hits
                    && f == folder
                    && *w == words
                {
                    for (_, uids) in hits {
                        found.extend(store.by_uids(id, uids).unwrap_or_default());
                    }
                }
                tidy_results(found, &self.folders)
            };
        }
    }

    /// The messages an action works on: the picked ones, else the selected one.
    fn targets(&self) -> Vec<(String, u32)> {
        if self.picked.len() > 1 {
            self.picked.clone()
        } else if let Some(h) = self.head_selected() {
            h.iter().filter(|x| !self.is_extra(x)).cloned().collect()
        } else {
            self.selected.clone().into_iter().collect()
        }
    }

    /// The conversation whose heading is selected (its newest message is the one open).
    fn head_selected(&self) -> Option<&Vec<(String, u32)>> {
        let h = self.head.as_ref()?;
        let sel = self.selected.as_ref()?;
        (self.picked.len() <= 1 && h.contains(sel)).then_some(h)
    }

    /// The messages a list line stands for.
    fn line_items(&self, line: &Line) -> Vec<(String, u32)> {
        self.line_rows(line).into_iter().map(|m| (m.folder.clone(), m.uid)).collect()
    }

    /// The messages a list line shows (none for a date heading).
    fn line_rows(&self, line: &Line) -> Vec<&MessageRow> {
        let idx: &[usize] = match line {
            Line::Msg { i, .. } => std::slice::from_ref(i),
            Line::Head { members, .. } => members,
            Line::Day(_) => &[],
        };
        idx.iter().filter_map(|&i| self.row(i)).collect()
    }

    /// The names of the files attached to what a line shows.
    fn line_files(&self, line: &Line) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        for m in self.line_rows(line) {
            for a in m.attachments.iter().flatten() {
                if !out.contains(a) {
                    out.push(a.clone());
                }
            }
        }
        out
    }

    /// The Focused Inbox is in use here: the Inbox, not searching.
    fn focused_here(&self) -> bool {
        self.settings.focused_inbox
            && self.vfolder.is_none()
            && self.search.trim().is_empty()
            && self.folder.as_deref().is_some_and(|f| self.kind_of(f) == foldertree::Kind::Inbox)
    }

    /// Does a message belong under Other? Senders put in Focused or Other stay there; else
    /// bulk mail (newsletters, notifications) from someone you have never written to.
    fn is_other(&self, m: &MessageRow) -> bool {
        let addr = m.from_addr.as_deref().unwrap_or("").to_ascii_lowercase();
        if settings::Settings::listed(&self.settings.focused_senders, &addr) {
            return false;
        }
        if settings::Settings::listed(&self.settings.other_senders, &addr) {
            return true;
        }
        m.bulk && !self.known.contains(&addr)
    }

    /// Everyone written to from this account (its Sent folder), for the Focused Inbox and
    /// automatic replies.
    fn refresh_known(&mut self) {
        self.known.clear();
        self.people.clear();
        let (Some(store), Some(acct)) = (&self.store, self.current()) else { return };
        let mut written: std::collections::HashMap<String, i64> = Default::default();
        if let Some(sent) = self.sent_name()
            && let Ok(Some(id)) = store.folder_id(&acct.email, &sent)
        {
            for m in store.messages(id, 5000).unwrap_or_default() {
                for a in m.to.as_deref().unwrap_or("").split(',') {
                    let a = a.trim().to_ascii_lowercase();
                    if !a.is_empty() {
                        *written.entry(a).or_default() += 1;
                    }
                }
            }
        }
        let senders = store.senders(&acct.email).unwrap_or_default();
        let written: Vec<(String, i64)> = written.into_iter().collect();
        self.people = people::gather(&acct.email, &senders, &written);
        self.known = written.into_iter().map(|(a, _)| a).collect();
    }

    /// A message by list index: the folder's own, then the Sent messages shown in conversations.
    fn row(&self, i: usize) -> Option<&MessageRow> {
        match i.checked_sub(self.messages.len()) {
            None => self.messages.get(i),
            Some(j) => self.extra.get(j),
        }
    }

    fn is_extra(&self, item: &(String, u32)) -> bool {
        self.extra.iter().any(|m| m.uid == item.1 && m.folder == item.0)
    }

    /// The messages a line stands for that belong to the list (not Sent copies shown alongside):
    /// what actions on the line touch, as in Outlook.
    fn line_items_here(&self, line: &Line) -> Vec<(String, u32)> {
        self.line_items(line).into_iter().filter(|x| !self.is_extra(x)).collect()
    }

    /// Conversation view in the open folder: its own setting, else the one for all folders.
    fn threads_on(&self) -> bool {
        if self.search_all && !self.search.trim().is_empty() {
            return self.threads;
        }
        let (Some(acct), Some(folder)) = (self.current(), self.folder.as_ref()) else { return self.threads };
        self.conv_folders.iter().find(|(r, _)| r.account == acct.email && r.folder == *folder).map(|(_, on)| *on).unwrap_or(self.threads)
    }

    /// The account's Sent folder, if it has one.
    fn sent_name(&self) -> Option<String> {
        self.folders.iter().find(|f| foldertree::kind(f) == foldertree::Kind::Sent).map(|f| f.name.clone())
    }

    fn line_selected(&self, line: &Line) -> bool {
        match line {
            Line::Head { .. } => {
                let items = self.line_items(line);
                self.head_selected().is_some_and(|h| *h == items) || (self.picked.len() > 1 && items.iter().all(|x| self.picked.contains(x)))
            }
            Line::Msg { i, .. } => self.head_selected().is_none() && self.row(*i).is_some_and(|m| self.is_picked(&m.folder, m.uid)),
            Line::Day(_) => false,
        }
    }

    /// The line the selection is on (a conversation heading when that is selected).
    fn current_line(&self) -> Option<usize> {
        if let Some(h) = self.head_selected() {
            return self.lines.iter().position(|l| matches!(l, Line::Head { .. }) && self.line_items(l) == *h);
        }
        let sel = self.selected.as_ref()?;
        let on = |l: &Line| matches!(l, Line::Msg { .. }) && self.line_items(l).first() == Some(sel);
        self.lines.iter().position(on)
    }

    /// A double-click on a list line: the message (a conversation's newest) in its own tab, filling
    /// the window, as Outlook opens a message window. A second double-click goes back to that tab.
    fn open_in_tab(&mut self, li: usize) {
        let Some(line) = self.lines.get(li).cloned() else { return };
        let Some(row) = self.line_rows(&line).into_iter().next() else { return };
        let (folder, uid) = (row.folder.clone(), row.uid);
        let subject = row.subject.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "(no subject)".into());
        let i = match self.tabs.iter().position(|t| matches!(t, MainTab::Message { folder: f, uid: u, .. } if *f == folder && *u == uid)) {
            Some(i) => i,
            None => {
                self.tabs.push(MainTab::Message { folder: folder.clone(), uid, subject });
                self.tabs.len() - 1
            }
        };
        self.active_tab = i;
        if self.open.as_ref().map(|o| (o.folder.as_str(), o.uid)) != Some((folder.as_str(), uid)) {
            self.open_message(folder, uid);
        }
    }

    /// A click on a list line: plain, Ctrl (1), Shift (2), or on a conversation's arrow (3).
    fn click_line(&mut self, li: usize, how: u8) {
        let Some(line) = self.lines.get(li).cloned() else { return };
        let items = self.line_items(&line);
        let here = self.line_items_here(&line);
        match how {
            3 => {
                if let Line::Head { key, open, .. } = &line {
                    if *open {
                        self.expanded.remove(key);
                    } else {
                        self.expanded.insert(key.clone());
                    }
                    self.arrange();
                }
            }
            1 => {
                // Ctrl: the whole line in or out of the selection.
                let mut set = self.targets();
                if here.iter().all(|x| set.contains(x)) {
                    set.retain(|x| !here.contains(x));
                } else {
                    for x in &here {
                        if !set.contains(x) {
                            set.push(x.clone());
                        }
                    }
                }
                self.head = None;
                self.set_picked(set, items.first().cloned());
            }
            2 => {
                // Shift: every line from the current one to this.
                let from = self.current_line().unwrap_or(0);
                let (lo, hi) = (from.min(li), from.max(li));
                let mut set: Vec<(String, u32)> = vec![];
                for l in self.lines.iter().skip(lo).take(hi - lo + 1) {
                    for x in self.line_items_here(l) {
                        if !set.contains(&x) {
                            set.push(x);
                        }
                    }
                }
                self.picked = set;
                if self.picked.len() <= 1 {
                    self.picked.clear();
                    if let Some((f, u)) = items.first().cloned() {
                        self.open_message(f, u);
                    }
                }
            }
            _ => match &line {
                // A conversation: open its newest message, select it all, and show its messages.
                Line::Head { key, .. } => {
                    if let Some((f, u)) = items.first().cloned() {
                        self.open_message(f, u);
                    }
                    self.head = Some(items);
                    if self.expanded.insert(key.clone()) {
                        self.arrange();
                    }
                }
                Line::Msg { .. } => {
                    if let Some((f, u)) = items.first().cloned() {
                        self.open_message(f, u);
                    }
                }
                Line::Day(_) => {}
            },
        }
    }

    fn is_picked(&self, folder: &str, uid: u32) -> bool {
        if self.picked.len() > 1 {
            self.picked.iter().any(|(f, u)| *u == uid && f == folder)
        } else {
            self.selected.as_ref().is_some_and(|(f, u)| *u == uid && f == folder)
        }
    }

    /// Take messages out of the list at once (the server catches up in the background), and
    /// open the one after them (else the one before), as Outlook does.
    fn take_out(&mut self, gone: &[(String, u32)]) {
        let is_gone = |m: &MessageRow| gone.iter().any(|(f, u)| *u == m.uid && *f == m.folder);
        let at: Vec<usize> = self.messages.iter().enumerate().filter(|(_, m)| is_gone(m)).map(|(i, _)| i).collect();
        // What opens next: Settings > Message handling (next, previous, or back to the list).
        let after = at.iter().max().and_then(|&last| self.messages.iter().skip(last + 1).find(|m| !is_gone(m)));
        let before = at.iter().min().and_then(|&first| self.messages.iter().take(first).rev().find(|m| !is_gone(m)));
        let next = match self.settings.after_delete {
            settings::AfterDelete::OpenNext => after.or(before),
            settings::AfterDelete::OpenPrevious => before.or(after),
            settings::AfterDelete::ReturnToList => None,
        }
        .map(|m| (m.folder.clone(), m.uid));
        if let (Some(store), Some(acct)) = (&self.store, self.accounts.get(self.account)) {
            for (folder, uids) in by_folder(gone) {
                if let Ok(Some(id)) = store.folder_id(&acct.email, &folder) {
                    let _ = store.remove(id, &uids);
                }
            }
        }
        self.picked.clear();
        // A message's own tab closes with it (back to the mail when it was the one showing).
        let gone_tab = |t: &MainTab| matches!(t, MainTab::Message { folder, uid, .. } if gone.iter().any(|(f, u)| f == folder && u == uid));
        if self.tabs.iter().any(gone_tab) {
            let was_active = self.tabs.get(self.active_tab).is_some_and(gone_tab);
            let keep_active = self.tabs.get(self.active_tab).cloned();
            self.tabs.retain(|t| !gone_tab(t));
            self.active_tab = if was_active {
                0
            } else {
                keep_active
                    .and_then(|k| self.tabs.iter().position(|t| std::mem::discriminant(t) == std::mem::discriminant(&k) && same_tab(t, &k)))
                    .unwrap_or(0)
            };
        }
        let had_selection = self.selected.as_ref().is_some_and(|(f, u)| gone.iter().any(|(g, v)| g == f && v == u));
        if had_selection {
            self.selected = None;
            self.open = None;
        }
        // The folder tree's counts come from this PC's copy too: they change now, not after
        // the server has been told.
        self.refresh_keeping_place();
        if had_selection && let Some((f, u)) = next {
            self.open_message(f, u);
        }
    }

    fn password(&mut self) -> Option<(Account, String)> {
        let acct = self.current().cloned()?;
        match secrets::get(&acct.email) {
            Some(pw) => Some((acct, pw)),
            None => {
                self.prompt = Some((self.account, String::new()));
                None
            }
        }
    }

    fn delete_selected(&mut self) {
        let targets = self.targets();
        self.delete_items(targets);
    }

    fn delete_items(&mut self, targets: Vec<(String, u32)>) {
        let Some((acct, pw)) = self.password().filter(|_| !targets.is_empty()) else { return };
        let trash = self.trash_name();
        self.note_undo(&acct.email, trash.as_deref(), &targets);
        for (folder, uids) in by_folder(&targets) {
            // Into Trash: where from, for Restore (noted before they leave this PC's copy). Out of
            // Trash for good: forgotten.
            self.note_origins(&acct.email, &folder, &uids, trash.as_deref() != Some(folder.as_str()));
            if self.settings.read_on_delete {
                let _ = self.worker.send(Request::SetFlag(acct.clone(), pw.clone(), folder.clone(), uids.clone(), "\\Seen".into(), true));
            }
            let _ = self.worker.send(Request::Delete(acct.clone(), pw.clone(), folder, uids));
        }
        self.status = "Deleting...".into();
        self.take_out(&targets);
    }

    /// Remember a delete into Trash for Ctrl+Z, by the messages' Message-IDs (their UIDs change
    /// on the way). Deletes from Trash itself are for good and cannot be undone.
    fn note_undo(&mut self, email: &str, trash: Option<&str>, items: &[(String, u32)]) {
        let (Some(trash), Some(store)) = (trash, &self.store) else { return };
        let mut undo = Undo { account: email.to_string(), ids: vec![], without_id: 0 };
        for (folder, uids) in by_folder(items) {
            if folder == trash {
                continue;
            }
            let ids = store.folder_id(email, &folder).ok().flatten().and_then(|id| store.message_ids(id, &uids).ok()).unwrap_or_default();
            undo.without_id += uids.len().saturating_sub(ids.len());
            undo.ids.extend(ids);
        }
        if undo.ids.is_empty() && undo.without_id == 0 {
            return;
        }
        self.undo.push(undo);
        if self.undo.len() > UNDO_KEEP {
            self.undo.remove(0);
        }
    }

    /// Ctrl+Z: the last delete goes back where it came from, as Restore does it from Trash. When
    /// the delete is still on its way to the server, the undo waits for it.
    fn undo_delete(&mut self) {
        let Some(u) = self.undo.pop() else {
            log::info!("undo: nothing to undo");
            self.status = "Nothing to undo.".into();
            return;
        };
        if self.current().is_none_or(|a| a.email != u.account) {
            self.status = format!("To undo that delete, go back to {}.", u.account);
            self.undo.push(u);
            return;
        }
        if u.ids.is_empty() {
            self.status = "That delete cannot be undone here: restore it from Trash.".into();
            return;
        }
        log::info!("{}: undo the delete of {} message(s)", u.account, u.ids.len());
        let without = u.without_id;
        self.undo_wait = None;
        self.finish_undo(u, std::time::Instant::now());
        if without > 0 {
            self.status =
                format!("{} ({without} without a Message-ID: restore {} from Trash.)", self.status, if without == 1 { "it" } else { "them" });
        }
    }

    /// Restore what of an undo has reached Trash on this PC; the rest waits (`undo_wait`) for
    /// the delete to finish, for up to a minute.
    fn finish_undo(&mut self, u: Undo, since: std::time::Instant) {
        let Some(trash) = self.trash_name() else { return };
        let trash_id = self.store.as_ref().and_then(|st| st.folder_id(&u.account, &trash).ok().flatten());
        let found = self.store.as_ref().zip(trash_id).and_then(|(st, id)| st.find_message_ids(id, &u.ids).ok()).unwrap_or_default();
        // None in Trash, all of them still (or again) in their folders: the delete never
        // happened (it failed, and the sync put them back), so there is nothing to undo.
        if found.is_empty()
            && let Some((st, id)) = self.store.as_ref().zip(trash_id)
            && st.message_ids_elsewhere(&u.account, id, &u.ids).ok() == Some(u.ids.len())
        {
            log::info!("{}: undo: the {} message(s) were not deleted", u.account, u.ids.len());
            self.status = "Nothing to undo: that mail is still where it was.".into();
            return;
        }
        let (here, rest) = split_undo(u, &found);
        if !here.is_empty() {
            self.restore_items(here.into_iter().map(|uid| (trash.clone(), uid)).collect());
        }
        if let Some(rest) = rest {
            if since.elapsed() < UNDO_WAIT {
                if found.is_empty() {
                    self.status = "Undoing... (the delete is still reaching the server)".into();
                }
                self.undo_wait = Some((rest, since, std::time::Instant::now()));
            } else {
                log::warn!("{}: undo gave up on {} message(s) not in Trash", rest.account, rest.ids.len());
                self.status = "Some of that delete could not be undone: look in Trash.".into();
            }
        }
    }

    /// Remember where messages going into Trash came from (`into_trash`), or forget it for
    /// messages leaving Trash; a failure here only loses the Restore, so it is just logged.
    fn note_origins(&self, email: &str, folder: &str, uids: &[u32], into_trash: bool) {
        let Some(store) = &self.store else { return };
        let Ok(Some(id)) = store.folder_id(email, folder) else { return };
        let r = if into_trash { store.remember_origin(id, uids) } else { store.forget_origins_in(id, Some(uids)) };
        if let Err(e) = r {
            log::warn!("{email}: keeping where messages in {folder} were deleted from: {e}");
        }
    }

    /// Move (or copy) the given messages to `to`; ones already there stay put.
    fn move_to(&mut self, items: Vec<(String, u32)>, to: String, copy: bool) {
        let items: Vec<(String, u32)> = items.into_iter().filter(|(f, _)| *f != to).collect();
        let Some((acct, pw)) = self.password().filter(|_| !items.is_empty()) else { return };
        let trash = self.trash_name();
        if !copy && trash.as_deref() == Some(to.as_str()) {
            self.note_undo(&acct.email, trash.as_deref(), &items);
        }
        for (from, uids) in by_folder(&items) {
            // Dragged into Trash is deleted too (Restore knows where from); moved out of it, the
            // record has done its job.
            if !copy && trash.as_deref() == Some(to.as_str()) {
                self.note_origins(&acct.email, &from, &uids, true);
            } else if !copy && trash.as_deref() == Some(from.as_str()) {
                self.note_origins(&acct.email, &from, &uids, false);
            }
            let r = if copy {
                Request::Copy(acct.clone(), pw.clone(), from, uids, to.clone())
            } else {
                Request::Move(acct.clone(), pw.clone(), from, uids, to.clone())
            };
            let _ = self.worker.send(r);
        }
        if copy {
            self.status = "Copying...".into();
        } else {
            self.status = "Moving...".into();
            self.take_out(&items);
        }
    }

    fn is_seen(&self, folder: &str, uid: u32) -> bool {
        self.messages.iter().any(|m| m.uid == uid && m.folder == folder && m.seen())
    }

    /// Read / unread: if any of them is unread, all become read; otherwise all unread.
    fn toggle_read(&mut self) {
        let targets = self.targets();
        self.toggle_read_items(&targets);
    }

    fn toggle_read_items(&mut self, targets: &[(String, u32)]) {
        let on = targets.iter().any(|(f, u)| !self.is_seen(f, *u));
        self.set_flag_on(targets, "\\Seen", on);
    }

    fn set_flag_on(&mut self, targets: &[(String, u32)], flag: &str, on: bool) {
        let Some((acct, pw)) = self.password().filter(|_| !targets.is_empty()) else { return };
        self.flag_here(&acct.email, targets, flag, on);
        for (folder, uids) in by_folder(targets) {
            let _ = self.worker.send(Request::SetFlag(acct.clone(), pw.clone(), folder, uids, flag.to_string(), on));
        }
    }

    /// Change a flag in this PC's copy and on screen at once; the server is told after, in the
    /// background, and a refusal puts the copy back right with a sync.
    fn flag_here(&mut self, email: &str, targets: &[(String, u32)], flag: &str, on: bool) {
        if let Some(store) = &self.store {
            for (folder, uids) in by_folder(targets) {
                if let Ok(Some(id)) = store.folder_id(email, &folder) {
                    for uid in uids {
                        let _ = store.change_flag(id, uid, flag, on);
                    }
                }
            }
        }
        self.refresh_keeping_place();
    }

    /// Re-read the folders and list from the cache, staying on the same folder.
    fn refresh_keeping_place(&mut self) {
        let keep = self.folder.clone();
        self.refresh_folders();
        self.folder = keep;
        self.refresh_messages();
    }

    /// F5, Alt+Left / Alt+Right and the mouse's back and forward buttons, on the mail (not a
    /// message's own tab, Settings or the calendar). Not while writing: the caller leaves the
    /// editor its keys. The keys wait while a box is being typed in, as a browser's do; the
    /// mouse buttons do not.
    fn folder_keys(&mut self, ctx: &egui::Context, typing: bool) -> Option<Action> {
        use egui::{Key, Modifiers as M, PointerButton as B};
        if !matches!(self.tabs.get(self.active_tab), Some(MainTab::Mail) | None) {
            return None;
        }
        let keys = !typing && self.settings.key_set != settings::KeySet::Off;
        ctx.input_mut(|i| {
            if i.pointer.button_pressed(B::Extra1) {
                Some(Action::GoBack)
            } else if i.pointer.button_pressed(B::Extra2) {
                Some(Action::GoForward)
            } else if !keys {
                None
            } else if i.consume_key(M::NONE, Key::F5) {
                Some(Action::RefreshFolder)
            } else if i.consume_key(M::COMMAND, Key::Z) {
                Some(Action::Undo)
            } else if i.consume_key(M::ALT, Key::ArrowLeft) {
                Some(Action::GoBack)
            } else if i.consume_key(M::ALT, Key::ArrowRight) {
                Some(Action::GoForward)
            } else {
                None
            }
        })
    }

    /// Remember the folder showing, for Back and Forward (once a frame: whichever way it was
    /// chosen, the folder pane, Favourites, a notification...). Search folders are not folders.
    fn note_visit(&mut self) {
        if self.vfolder.is_some() {
            return;
        }
        let (Some(acct), Some(f)) = (self.accounts.get(self.account), self.folder.as_ref()) else { return };
        if self.history.is_at(&acct.email, f) || !self.folders.iter().any(|x| x.name == *f) {
            return;
        }
        self.history.visit(history::Visit { account: acct.email.clone(), folder: f.clone() });
    }

    /// Whether a remembered folder is still there: its account still added, the folder still in
    /// this PC's copy of the account (deleted or renamed ones are stepped over).
    fn visit_exists(&self, v: &history::Visit) -> bool {
        if self.current().is_some_and(|a| a.email == v.account) {
            return self.folders.iter().any(|f| f.name == v.folder);
        }
        self.accounts.iter().any(|a| a.email == v.account)
            && self.store.as_ref().is_some_and(|st| st.folder_id(&v.account, &v.folder).ok().flatten().is_some())
    }

    /// Back (or forward) to the folder shown before (or after), as a browser goes between pages.
    fn go_back_or_forward(&mut self, forward: bool) {
        let mut h = std::mem::take(&mut self.history);
        let to = if forward { h.forward(|v| self.visit_exists(v)) } else { h.back(|v| self.visit_exists(v)) };
        self.history = h;
        let Some(v) = to else { return };
        log::info!("{}: {} to {}", v.account, if forward { "forward" } else { "back" }, v.folder);
        let Some(idx) = self.accounts.iter().position(|a| a.email == v.account) else { return };
        let other = idx != self.account;
        self.account = idx;
        self.vfolder = None;
        self.folder = Some(v.folder);
        self.selected = None;
        self.picked.clear();
        self.open = None;
        self.search.clear();
        self.server_hits = None;
        if other {
            self.folders.clear();
            self.refresh_folders();
        } else {
            self.refresh_messages();
        }
    }

    /// F5: bring the folder showing up to date with the server, in the background; the list
    /// stays where it is and is read again when the folder is done. A search folder spans the
    /// account, so for one of those every folder is synced.
    fn refresh_folder(&mut self) {
        let Some(acct) = self.current().cloned() else { return };
        let Some(folder) = self.folder.clone().filter(|_| self.vfolder.is_none()) else {
            if !self.syncing {
                log::info!("{}: refresh a search folder: syncing every folder", acct.email);
                self.sync(self.account);
            }
            return;
        };
        let label = folder_label(&folder, &self.folders);
        let key = (acct.email.clone(), folder.clone());
        if self.refreshing.contains(&key) {
            // F5 held down repeats: one refresh at a time is enough.
            self.status = format!("Refreshing {label}...");
            return;
        }
        let Some(pw) = secrets::get(&acct.email) else {
            self.prompt = Some((self.account, String::new()));
            return;
        };
        // What it brings is new mail, announced as a sync's is; a sync already running keeps
        // its own marks (and announces this folder's mail too).
        if !self.syncing
            && let Some(marks) = self.store.as_ref().and_then(|st| st.max_uids(&acct.email).ok())
        {
            self.new_mail_marks.entry(acct.email.clone()).or_insert(marks);
        }
        log::info!("{}: refresh {folder} (F5)", acct.email);
        self.refreshing.insert(key);
        self.status = format!("Refreshing {label}...");
        let _ = self.worker.send(Request::RefreshFolder(acct, pw, folder));
    }

    /// A folder refreshed (F5): say so, read the folder tree and list again from this PC's copy.
    fn folder_refreshed(&mut self, email: String, folder: String, r: Result<(mailcraft_core::sync::Report, Option<String>), String>) {
        self.refreshing.remove(&(email.clone(), folder.clone()));
        let label = folder_label(&folder, &self.folders);
        // The last of this account's refreshes announces the new mail (unless a sync will).
        let last = !self.refreshing.iter().any(|(e, _)| *e == email);
        self.status = match r {
            Ok((rep, rules)) => {
                if last && !self.syncing {
                    if self.started.elapsed() > std::time::Duration::from_secs(20) {
                        self.announce_new_mail(&email);
                    } else {
                        self.new_mail_marks.remove(&email);
                    }
                }
                let mut msg = if rep.folders == 0 {
                    format!("{label} is not on this PC yet: it comes with the next Send/Receive.")
                } else {
                    format!("{label} is up to date: {} new, {} removed.", rep.new_messages, rep.removed)
                };
                if let Some(r) = rules {
                    msg.push_str(&format!(" {r}."));
                }
                msg
            }
            Err(e) => {
                if last && !self.syncing {
                    self.new_mail_marks.remove(&email);
                }
                self.needs_sign_in(&email, &e);
                let msg = format!("{label} could not be refreshed: {}", e.trim_start_matches(worker::SIGN_IN_AGAIN));
                self.notice(&msg, true);
                msg
            }
        };
        self.refresh_keeping_place();
    }

    /// The account's Trash, as the worker finds it (without asking the server).
    fn trash_name(&self) -> Option<String> {
        mailcraft_core::manage::find_trash(&self.folders)
    }

    fn restore_selected(&mut self) {
        let targets = self.targets();
        self.restore_items(targets);
    }

    /// Put messages in Trash back in the folders they were deleted from, at once on this PC; the
    /// server follows in the background, as for a delete. Where MailCraft did not see them go
    /// (deleted before it kept track, or by another app), or the folder has gone, they go to
    /// the Inbox, and the status bar says so.
    fn restore_items(&mut self, items: Vec<(String, u32)>) {
        let Some(trash) = self.trash_name() else { return };
        let items: Vec<(String, u32)> = items.into_iter().filter(|(f, _)| *f == trash).collect();
        let Some((acct, pw)) = self.password().filter(|_| !items.is_empty()) else { return };
        let uids: Vec<u32> = items.iter().map(|(_, u)| *u).collect();
        let origins = self
            .store
            .as_ref()
            .and_then(|st| st.folder_id(&acct.email, &trash).ok().flatten().and_then(|id| st.origins(id, &uids).ok()))
            .unwrap_or_default();
        let inbox =
            self.folders.iter().find(|f| foldertree::kind(f) == foldertree::Kind::Inbox).map(|f| f.name.clone()).unwrap_or_else(|| "INBOX".into());
        let mut plan =
            plan_restore(&trash, &uids, &origins, &|f| self.folders.iter().any(|x| x.name == f), &inbox, &|f| folder_label(f, &self.folders));
        // The move that went to the Inbox for want of knowing better last: its answer, which
        // says so, is what the status bar is left showing.
        plan.sort_by_key(|r| r.unknown > 0);
        let unknown: usize = plan.iter().map(|r| r.unknown).sum();
        log::info!(
            "{}: restore {} from {trash} to {} ({unknown} to {inbox}: where they came from is not known)",
            acct.email,
            uids.len(),
            plan.iter().map(|r| r.to.as_str()).collect::<Vec<_>>().join(", ")
        );
        for r in plan {
            let _ = self.worker.send(Request::Restore(acct.clone(), pw.clone(), r));
        }
        let inbox = folder_label(&inbox, &self.folders);
        self.status = match (unknown, uids.len()) {
            (0, _) => "Restoring...".into(),
            (1, 1) => format!("Restoring to {inbox}: where it was deleted from is not known."),
            (u, n) if u >= n => format!("Restoring to {inbox}: where they were deleted from is not known."),
            (u, _) => format!("Restoring... {u} of them to {inbox}: where they were deleted from is not known."),
        };
        self.take_out(&items);
    }

    fn server_search(&mut self) {
        let words = self.search.trim().to_string();
        self.settings.remember_search(&words);
        let _ = settings::save(&self.settings);
        let Some(acct) = self.current().cloned() else { return };
        let scope = if self.search_all { None } else { self.folder.clone() };
        // A search already running is replaced by this one (the worker stops the old one).
        if words.is_empty() || (!self.search_all && scope.is_none()) {
            return;
        }
        let Some(pw) = secrets::get(&acct.email) else { return };
        self.searching = true;
        self.search_progress = None;
        self.status = match &scope {
            Some(f) => format!("Searching {f} on the server..."),
            None => "Searching every folder on the server...".into(),
        };
        let _ = self.worker.send(Request::Search(acct, pw, scope, words));
    }

    /// What has to happen even while the window is minimised, when only `logic` runs (eframe
    /// draws nothing then): the taskbar flash, a card as a Windows notification instead, and
    /// clicks on those notifications.
    fn notify_while_hidden(&mut self, ctx: &egui::Context) {
        self.hidden = ctx.input(|i| i.viewport().minimized == Some(true));
        // New mail (or a reminder) while MailCraft is not in front: flash the taskbar (no sound).
        if std::mem::take(&mut self.alert) && ctx.input(|i| i.viewport().focused) != Some(true) {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        }
        if self.hidden
            && let Some(card) = self.card.take()
            && !notify::show(&card, self.notified.0.clone(), ctx.clone())
        {
            // Not possible here: keep it for when the window is back.
            self.card = Some(card);
        }
        while let Ok(c) = self.notified.1.try_recv() {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
            self.card_open = c.open;
            self.card_day = c.day;
        }
    }

    /// Download mail to this PC as Settings > Storage says (after a sync, or when the setting
    /// changes), for every account whose password is to hand.
    pub(crate) fn start_download(&mut self) {
        if self.settings.offline == settings::Offline::Opened {
            let _ = self.worker.send(Request::StopDownload);
            self.offline_progress = None;
            return;
        }
        for email in self.accounts.iter().map(|a| a.email.clone()).collect::<Vec<_>>() {
            self.download_account(&email);
        }
    }

    /// Messages the search cannot see into yet (not downloaded) in its scope; counted at most
    /// every two seconds, as this is asked every frame while searching.
    fn missing_here(&mut self) -> i64 {
        let scope = if self.search_all { None } else { self.folder.clone() };
        if let Some((s, at, n)) = &self.missing_count
            && *s == scope
            && at.elapsed() < std::time::Duration::from_secs(2)
        {
            return *n;
        }
        let n = self
            .current()
            .zip(self.store.as_ref())
            .and_then(|(a, st)| st.downloaded(&a.email, scope.as_deref()).ok())
            .map_or(0, |(here, all)| all - here);
        self.missing_count = Some((scope, std::time::Instant::now(), n));
        n
    }

    fn download_account(&mut self, email: &str) {
        let since = match self.settings.offline {
            settings::Offline::All => None,
            settings::Offline::Year => Some(mailcraft_core::sync::utc(chrono::Utc::now().timestamp() - 365 * 86_400)),
            settings::Offline::Opened => return,
        };
        let Some(acct) = self.accounts.iter().find(|a| a.email == email).cloned() else { return };
        let Some(pw) = secrets::get(&acct.email) else { return };
        let _ = self.worker.send(Request::Download(acct, pw, since));
    }

    /// Stop the server search: Stop pressed, or the search box emptied.
    fn stop_search(&mut self) {
        if std::mem::take(&mut self.searching) {
            let _ = self.worker.send(Request::CancelSearch);
            self.search_progress = None;
            self.status = "Server search stopped.".into();
        }
    }

    /// The action a key press asks for: Outlook's keys, or the set chosen in Settings >
    /// Accessibility (Outlook on the web, Gmail, Yahoo! Mail, or none).
    fn shortcut(&mut self, ctx: &egui::Context) -> Option<Action> {
        let table = match self.settings.key_set {
            settings::KeySet::Outlook => return self.outlook_shortcut(ctx),
            settings::KeySet::Off => &[][..],
            settings::KeySet::OutlookWeb => OUTLOOK_WEB_KEYS,
            settings::KeySet::Gmail => GMAIL_KEYS,
            settings::KeySet::Yahoo => YAHOO_KEYS,
        };
        let unread = self.selected_row().is_some_and(|m| !m.seen());
        let does = ctx.input_mut(|i| {
            for (m, press, does, _, _) in table {
                let hit = match press {
                    Press::K(k) => i.consume_key(*m, *k),
                    Press::C(c) => {
                        let at = i.events.iter().position(|e| matches!(e, egui::Event::Text(t) if t == c));
                        at.map(|n| i.events.remove(n)).is_some()
                    }
                };
                if hit {
                    // The character the key typed goes too, or it lands in what the key opens.
                    i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
                    return Some(*does);
                }
            }
            if self.settings.key_set != settings::KeySet::Off && i.consume_key(NONE, egui::Key::F9) {
                return Some(Does::Act(Action::SendReceive));
            }
            if i.consume_key(NONE, egui::Key::ArrowDown) {
                return Some(Does::Nav(1));
            }
            if i.consume_key(NONE, egui::Key::ArrowUp) {
                return Some(Does::Nav(-1));
            }
            jump_key(i).map(Does::Jump)
        });
        match does? {
            Does::Act(a) => Some(a),
            Does::Read => unread.then_some(Action::ToggleRead),
            Does::Unread => (!unread).then_some(Action::ToggleRead),
            Does::Nav(n) => {
                self.step(n);
                None
            }
            Does::Jump(j) => {
                self.jump(j);
                None
            }
        }
    }

    /// Key tips (Settings > Accessibility): a tap of Alt, or F10, puts letters over the ribbon's
    /// tabs, then over the open tab's commands; typing them picks it, Esc steps back.
    fn keytip_keys(&mut self, ctx: &egui::Context) -> Option<Action> {
        if !self.settings.keytips {
            self.keytips.level = 0;
            return None;
        }
        // A tap: Alt down and up again with nothing else pressed in between (seen in the key
        // events, so a quick tap inside one frame counts too).
        let mut tapped = false;
        ctx.input(|i| {
            for e in &i.events {
                match e {
                    egui::Event::Key { key, pressed, .. } if key.name().contains("Alt") => {
                        if *pressed {
                            self.alt_clean = true;
                        } else if std::mem::take(&mut self.alt_clean) {
                            tapped = true;
                        }
                    }
                    egui::Event::Key { key, pressed: true, .. }
                        if !["Shift", "Control", "Ctrl", "Meta", "Super"].iter().any(|m| key.name().contains(m)) =>
                    {
                        self.alt_clean = false;
                    }
                    egui::Event::PointerButton { .. } => self.alt_clean = false,
                    _ => {}
                }
            }
        });
        let f10 = ctx.input_mut(|i| i.consume_key(NONE, egui::Key::F10));
        if tapped || f10 {
            self.keytips.level = if self.keytips.level == 0 { 1 } else { 0 };
            self.keytips.typed.clear();
            return None;
        }
        if self.keytips.level == 0 {
            return None;
        }
        // While the letters show, keys are theirs.
        let keys: Vec<egui::Key> = ctx.input_mut(|i| {
            let k = i.events.iter().filter_map(|e| if let egui::Event::Key { key, pressed: true, .. } = e { Some(*key) } else { None }).collect();
            i.events.retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)));
            k
        });
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.keytips.level = 0;
        }
        for key in keys {
            if key == egui::Key::Escape {
                self.keytips.level = self.keytips.level.saturating_sub(1);
                self.keytips.typed.clear();
                continue;
            }
            let name = key.name();
            if name.chars().count() != 1 {
                continue;
            }
            self.keytips.typed.push_str(&name.to_uppercase());
            let typed = self.keytips.typed.clone();
            match self.keytips.codes.iter().find(|(c, _)| *c == typed).map(|(_, t)| *t) {
                Some(ribbon::KeyTarget::Tab(tab)) => {
                    self.tab = tab;
                    if self.fold.collapsed {
                        self.fold.peek = true;
                    }
                    self.keytips.level = if tab == Tab::File { 0 } else { 2 };
                    self.keytips.typed.clear();
                }
                Some(ribbon::KeyTarget::Act(a)) => {
                    self.keytips.level = 0;
                    self.keytips.typed.clear();
                    return Some(a);
                }
                None => {
                    if !self.keytips.codes.iter().any(|(c, _)| c.starts_with(&typed)) {
                        self.keytips.typed.clear();
                    }
                }
            }
        }
        None
    }

    /// Outlook for Windows' keys.
    fn outlook_shortcut(&mut self, ctx: &egui::Context) -> Option<Action> {
        use egui::{Key, Modifiers as M};
        let cs = M::COMMAND | M::SHIFT;
        let unread = self.selected_row().is_some_and(|m| !m.seen());
        let mut nav = 0i32;
        let mut jump = None;
        let a = ctx.input_mut(|i| {
            // The Shift forms first: a plain Ctrl+R check would also take Ctrl+Shift+R.
            if i.consume_key(M::COMMAND, Key::F1) {
                Some(Action::ToggleRibbon)
            } else if i.consume_key(cs, Key::R) {
                Some(Action::ReplyAll)
            } else if i.consume_key(cs, Key::V) {
                Some(Action::MoveTo)
            } else if i.consume_key(cs, Key::E) {
                Some(Action::NewFolder)
            } else if i.consume_key(M::COMMAND, Key::A) {
                nav = 9;
                None
            } else if i.consume_key(M::SHIFT, Key::ArrowDown) {
                nav = 2;
                None
            } else if i.consume_key(M::SHIFT, Key::ArrowUp) {
                nav = -2;
                None
            } else if i.consume_key(M::COMMAND, Key::N) {
                Some(Action::NewMail)
            } else if i.consume_key(M::COMMAND, Key::R) {
                Some(Action::Reply)
            } else if i.consume_key(M::COMMAND, Key::F) {
                Some(Action::Forward)
            } else if i.consume_key(M::COMMAND, Key::E) {
                Some(Action::FocusSearch)
            } else if i.consume_key(M::COMMAND, Key::P) {
                Some(Action::Print)
            } else if i.consume_key(M::COMMAND, Key::Q) {
                unread.then_some(Action::ToggleRead)
            } else if i.consume_key(M::COMMAND, Key::U) {
                (!unread).then_some(Action::ToggleRead)
            } else if i.consume_key(M::NONE, Key::Delete) {
                Some(Action::Delete)
            } else if i.consume_key(M::NONE, Key::Insert) {
                Some(Action::ToggleFlag)
            } else if i.consume_key(M::NONE, Key::F9) {
                Some(Action::SendReceive)
            } else {
                if i.consume_key(M::NONE, Key::ArrowDown) {
                    nav = 1;
                } else if i.consume_key(M::NONE, Key::ArrowUp) {
                    nav = -1;
                } else {
                    jump = jump_key(i);
                }
                None
            }
        });
        match nav {
            9 => self.select_all(),
            2 => self.pick_extend(1),
            -2 => self.pick_extend(-1),
            1 | -1 => self.step(nav),
            _ => {}
        }
        if let Some(j) = jump {
            self.jump(j);
        }
        a
    }

    /// Open the next (1) or previous (-1) message in the list.
    fn step(&mut self, by: i32) {
        let mut next = match (self.current_line(), by > 0) {
            (None, _) => Some(0),
            (Some(i), true) => Some(i + 1),
            (Some(i), false) => i.checked_sub(1),
        };
        // Past the date headings.
        while let Some(n) = next
            && matches!(self.lines.get(n), Some(Line::Day(_)))
        {
            next = if by > 0 { Some(n + 1) } else { n.checked_sub(1) };
        }
        if let Some(n) = next.filter(|n| *n < self.lines.len()) {
            self.click_line(n, 0);
            self.list_reveal = Some(n);
        }
    }

    /// Page Up/Down (by the rows that fit, less one so a row stays in sight), Home and End: open
    /// that message, as the arrow keys do, and scroll the list to it.
    fn jump(&mut self, to: Jump) {
        let pickable = |l: &Line| !matches!(l, Line::Day(_));
        let first = self.lines.iter().position(pickable);
        let last = self.lines.iter().rposition(pickable);
        let (Some(first), Some(last)) = (first, last) else { return };
        let (_, view_h, row_h) = self.list_view;
        let page = if row_h > 0.0 { ((view_h / row_h).floor() as usize).saturating_sub(1).max(1) } else { 10 };
        let here = self.current_line();
        let mut n = match to {
            Jump::Home => first,
            Jump::End => last,
            Jump::PageDown => here.map_or(first, |h| (h + page).min(last)),
            Jump::PageUp => here.map_or(first, |h| h.saturating_sub(page).max(first)),
        };
        // Off a date heading: on down the list for Page Down, back up for Page Up.
        while matches!(self.lines.get(n), Some(Line::Day(_))) {
            n = match to {
                Jump::PageUp | Jump::End => n - 1,
                _ => n + 1,
            };
        }
        if here != Some(n) {
            self.click_line(n, 0);
        }
        self.list_reveal = Some(n);
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn hide_web(&mut self) {
        if let Some(w) = self.web.as_mut() {
            w.hide();
        }
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    fn hide_web(&mut self) {}

    /// Remember the layout when it changed: at most once a second while it keeps changing
    /// (dragging the window), and at once when closing.
    fn save_view(&mut self, ctx: &egui::Context, now: bool) {
        let v = View {
            tab: self.tab,
            ribbon_collapsed: self.fold.collapsed,
            ribbon_simplified: self.fold.simplified,
            reading_pane: self.pane_at,
            folder_pane: self.show_folders,
            sort: self.sort,
            reverse: self.reverse,
            search_all_folders: self.search_all,
            conversations: self.threads,
            conversation_folders: self.conv_folders.clone(),
            theme: self.theme,
            dark_messages: self.dark_messages,
            window: self.place,
            maximized: self.maximized,
            folder_pane_width: self.folder_w.round(),
            list_width: self.list_w.round(),
            from_column: self.col_w.get(&settings::ListCol::From).copied().unwrap_or(self.from_col).round(),
            column_widths: settings::ListCol::ALL.iter().filter_map(|c| self.col_w.get(c).map(|w| (*c, w.round()))).collect(),
            list_height: self.list_h.round(),
        };
        if self.saved_view.as_ref() != Some(&v) {
            let wait = std::time::Duration::from_secs(1);
            if now || self.view_written.elapsed() >= wait {
                let _ = accounts::write_settings("view.json", &v);
                self.saved_view = Some(v);
                self.view_written = std::time::Instant::now();
            } else {
                ctx.request_repaint_after(wait);
            }
        }
    }

    /// The theme in effect.
    fn look(&self) -> looks::Theme {
        self.applied.unwrap_or(looks::Theme::Light)
    }

    fn message_css(&self) -> &'static str {
        self.look().message_css(self.dark_messages)
    }

    fn dark_reading(&self) -> bool {
        self.look().dark_reading(self.dark_messages)
    }

    /// Show the open message again in the current theme.
    fn restyle_message(&mut self) {
        let css = self.message_css();
        let dark = self.dark_reading();
        if let Some(o) = self.open.as_mut() {
            o.prepared = htmlview::prepare(&reading_html(&format!("{}{}", o.html, o.thread), dark), &o.inline, o.show_remote, css);
        }
    }

    /// Put the chosen theme on the window when it (or, for System, the system's) changes.
    /// Text size and animations (Settings > Message list), when they change.
    fn apply_view(&mut self, ctx: &egui::Context) {
        let zoom = self.settings.text_size.zoom();
        if self.zoom_applied != Some(zoom) {
            ctx.set_zoom_factor(zoom);
            self.zoom_applied = Some(zoom);
        }
        let animate = match self.settings.animations {
            settings::Animations::On => true,
            settings::Animations::Off => false,
            settings::Animations::System => system_animations(),
        };
        if self.animations_applied != Some(animate) {
            ctx.global_style_mut(|s| s.animation_time = if animate { 1.0 / 12.0 } else { 0.0 });
            self.animations_applied = Some(animate);
        }
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let system_dark = ctx.system_theme() == Some(egui::Theme::Dark);
        let want = self.theme.resolve(system_dark);
        if self.applied == Some(want) {
            return;
        }
        let t = want.tokens();
        wordcraft_ui_egui::theme::apply(ctx, &t);
        if want.high_contrast() {
            looks::strengthen(ctx, &t);
        }
        self.applied = Some(want);
        self.animations_applied = None;
        self.restyle_message();
    }

    /// Things that happen with time: checking for mail, marking read after a while, closing
    /// once Trash is emptied.
    fn timers(&mut self, ctx: &egui::Context) {
        use std::time::{Duration, Instant};
        let every = self.settings.check_minutes;
        if every > 0 {
            let due = Duration::from_secs(every as u64 * 60);
            if self.last_check.elapsed() >= due {
                self.last_check = Instant::now();
                // Only accounts whose password or sign-in is at hand: a timer never prompts.
                let mut skipped = vec![];
                for i in 0..self.accounts.len() {
                    let Some(a) = self.accounts.get(i) else { continue };
                    if secrets::get(&a.email).is_some() {
                        self.sync(i);
                    } else {
                        skipped.push(a.email.clone());
                    }
                }
                if !skipped.is_empty() {
                    log::warn!("checking for mail: skipped {} (no saved password or sign-in at hand)", skipped.join(", "));
                }
                self.calendar_sync_all();
            }
            ctx.request_repaint_after(due.saturating_sub(self.last_check.elapsed()).max(Duration::from_secs(1)));
        }
        // The calendars: synced shortly after starting, and reminders when they fall due.
        if self.cal.last_sync_due(self.started) {
            self.calendar_sync_all();
        }
        self.calendar_timers(ctx);
        if let Some((key, since)) = self.read_timer.clone() {
            if self.selected.as_ref() != Some(&key) {
                self.read_timer = None;
            } else {
                let wait = Duration::from_secs(self.settings.mark_read_secs.max(1) as u64);
                if since.elapsed() >= wait {
                    self.read_timer = None;
                    self.mark_read_now(&key.0, key.1);
                } else {
                    ctx.request_repaint_after(wait - since.elapsed());
                }
            }
        }
        // Closing: empty Trash first when that is set (Settings > Message handling).
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.settings.empty_trash_on_exit && self.exiting.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            let mut n = 0;
            for a in self.accounts.clone() {
                if let Some(pw) = secrets::get(&a.email) {
                    let _ = self.worker.send(Request::Folder(a, pw, FolderOp::EmptyTrash));
                    n += 1;
                }
            }
            self.status = "Emptying Trash before closing...".into();
            self.exiting = Some((n, Instant::now()));
        }
        if let Some((left, since)) = self.exiting {
            if left == 0 || since.elapsed() > Duration::from_secs(10) {
                self.exiting = None;
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        }
    }

    fn row_unread(&self, folder: &str, uid: u32) -> bool {
        self.messages.iter().chain(&self.extra).any(|m| m.uid == uid && m.folder == folder && !m.seen())
    }

    fn mark_read_now(&mut self, folder: &str, uid: u32) {
        if let Some(acct) = self.current().cloned()
            && let Some(pw) = secrets::get(&acct.email)
        {
            self.flag_here(&acct.email, &[(folder.to_string(), uid)], "\\Seen", true);
            let _ = self.worker.send(Request::Flag(acct, pw, folder.to_string(), uid, "\\Seen".into()));
        }
    }

    /// Tell the sender the message was read: a short message to the address it gave.
    fn send_receipt(&mut self, (to, subject, date): (String, String, String)) {
        let Some((acct, pw)) = self.password() else { return };
        let now = chrono::Local::now().format("%d %B %Y %H:%M").to_string();
        let me = self.settings.name_for(&acct.email).unwrap_or_else(|| acct.email.clone());
        let text = format!("Your message\n\n  To: {me}\n  Subject: {subject}\n  Sent: {date}\n\nwas read on {now}.\n");
        let out = mailcraft_core::send::Outgoing {
            from: acct.email.clone(),
            from_name: self.settings.name_for(&acct.email),
            to: vec![to],
            subject: format!("Read: {subject}"),
            text,
            ..Default::default()
        };
        let _ = self.worker.send(Request::SendOnly(acct, pw, Box::new(out)));
        self.receipt = None;
    }

    /// New mail arrived: flash the taskbar and show a card or banners (Settings >
    /// Notifications). Never a sound. The Inbox, and with `notify_all_folders` every folder mail
    /// goes straight to (sorted by rules or the server): not Junk, Trash, Sent, Drafts or
    /// Archive, nor folders left out from their right-click menu, nor mail already read there.
    fn announce_new_mail(&mut self, email: &str) {
        let Some(marks) = self.new_mail_marks.remove(email) else { return };
        let s = &self.settings;
        if !s.notifications {
            return;
        }
        let who = email.to_string();
        let Some(store) = &self.store else { return };
        let mut new: Vec<MessageRow> = vec![];
        // Where it went: (folder as people see it, how many), for the card.
        let mut places: Vec<(String, usize)> = vec![];
        for f in store.folders(email).unwrap_or_default() {
            let kind = foldertree::kind(&f);
            let wanted = match kind {
                foldertree::Kind::Inbox => true,
                foldertree::Kind::Folder => s.notify_all_folders,
                _ => false,
            };
            if !wanted || self.folder_prefs.is_quiet(email, &f.name) {
                continue;
            }
            // A folder not in the cache before this sync: all of it would look new.
            let Some(&mark) = marks.get(&f.name) else { continue };
            let mut got = store.unread_above(f.id, mark, 50).unwrap_or_default();
            if kind == foldertree::Kind::Inbox && s.notify_focused_only && s.focused_inbox {
                got.retain(|m| !self.is_other(m));
            }
            if !got.is_empty() {
                let shown = if kind == foldertree::Kind::Inbox {
                    "Inbox".to_string()
                } else {
                    f.delimiter.as_deref().filter(|d| !d.is_empty()).and_then(|d| f.name.rsplit(d).next()).unwrap_or(&f.name).to_string()
                };
                places.push((shown, got.len()));
                new.extend(got);
            }
        }
        // "New mail · address", or the folder when it all went to one that is not the Inbox.
        let heading = match places.as_slice() {
            [(folder, _)] if folder != "Inbox" => format!("New mail in {folder} \u{00b7} {who}"),
            _ => format!("New mail \u{00b7} {who}"),
        };
        let where_line =
            (places.len() > 1).then(|| format!("In {}", places.iter().map(|(f, n)| format!("{f} ({n})")).collect::<Vec<_>>().join(", ")));
        if new.is_empty() {
            return;
        }
        if s.alert_new_mail {
            self.alert = true;
        }
        // Betterbird's card at the corner of the screen: one message, or how many and from whom.
        if s.notify_card {
            let card = match new.as_slice() {
                [m] => {
                    let mut lines = vec![m.subject.clone().unwrap_or_else(|| "(no subject)".into())];
                    if let Some(p) = m.preview.clone().filter(|p| !p.is_empty()) {
                        lines.push(p);
                    }
                    Card {
                        heading: heading.clone(),
                        title: m.from_name.clone().or(m.from_addr.clone()).unwrap_or_default(),
                        lines,
                        open: Some((m.folder.clone(), m.uid)),
                        day: None,
                        born: std::time::Instant::now(),
                    }
                }
                _ => {
                    let mut from: Vec<String> = vec![];
                    for m in &new {
                        let n = m.from_name.clone().or(m.from_addr.clone()).unwrap_or_default();
                        if !from.contains(&n) {
                            from.push(n);
                        }
                    }
                    let more = from.len().saturating_sub(3);
                    let mut line = format!("From {}", from.iter().take(3).cloned().collect::<Vec<_>>().join(", "));
                    if more > 0 {
                        line.push_str(&format!(" and {more} more"));
                    }
                    let subjects: Vec<String> = new.iter().take(2).filter_map(|m| m.subject.clone()).collect();
                    Card {
                        heading: heading.clone(),
                        title: format!("{} new messages", new.len()),
                        lines: std::iter::once(line).chain(where_line.clone()).chain(subjects).collect(),
                        open: new.first().map(|m| (m.folder.clone(), m.uid)),
                        day: None,
                        born: std::time::Instant::now(),
                    }
                }
            };
            self.card = Some(card);
            return;
        }
        if !s.notify_banner {
            return;
        }
        if new.len() > 3 {
            let from: Vec<String> = new.iter().take(3).map(|m| m.from_name.clone().or(m.from_addr.clone()).unwrap_or_default()).collect();
            self.banners.push(Banner {
                title: format!("{} new messages", new.len()),
                lines: vec![format!("From {} and others", from.join(", "))],
                open: None,
                born: std::time::Instant::now(),
                error: false,
                sticky: false,
            });
        } else {
            for m in new {
                let mut lines = vec![m.subject.clone().unwrap_or_else(|| "(no subject)".into())];
                if let Some(p) = m.preview.clone().filter(|p| !p.is_empty()) {
                    lines.push(p);
                }
                self.banners.push(Banner {
                    title: m.from_name.clone().or(m.from_addr.clone()).unwrap_or_default(),
                    lines,
                    open: Some((m.folder.clone(), m.uid)),
                    born: std::time::Instant::now(),
                    error: false,
                    sticky: false,
                });
            }
        }
        self.banners.truncate(6);
    }

    /// An error, or (when Settings > Accessibility keeps them) a confirmation, as a banner.
    fn notice(&mut self, text: &str, error: bool) {
        let stay = self.settings.notices_stay;
        if !error && !stay {
            return;
        }
        self.banners.retain(|b| b.title != text);
        self.banners.push(Banner { title: text.to_string(), lines: vec![], open: None, born: std::time::Instant::now(), error, sticky: stay });
        if self.banners.len() > 6 {
            self.banners.remove(0);
        }
    }

    /// The new-mail card: a small window of its own at the bottom right of the screen, over
    /// everything, off the taskbar and never taking the keyboard, as Betterbird's alert.
    /// It fades in, stays while the mouse is on it, and a click brings MailCraft forward with
    /// the message open.
    fn draw_card(&mut self, ctx: &egui::Context, t: &Tokens) {
        if let Some(target) = self.card_open.take() {
            self.open_notified(target);
        }
        if let Some(day) = self.card_day.take() {
            self.calendar_go_to(day);
        }
        let Some(card) = self.card.as_ref() else { return };
        let age = card.born.elapsed().as_secs_f32();
        if age > CARD_SECS {
            self.card = None;
            return;
        }
        if self.card_icon.is_none()
            && let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/app-icon/mailcraft-256.png"))
        {
            let img = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
            self.card_icon = Some(ctx.load_texture("mc_card_icon", img, egui::TextureOptions::LINEAR));
        }
        let size = vec2(380.0, 118.0);
        // The bottom right of the screen, above the taskbar.
        let screen = ctx.input(|i| i.viewport().monitor_size).unwrap_or(vec2(1920.0, 1080.0));
        let at = pos2((screen.x - size.x - 16.0).max(0.0), (screen.y - size.y - 60.0).max(0.0));
        let fade = (age / CARD_FADE).min(1.0).min((CARD_SECS - age) / CARD_FADE).clamp(0.0, 1.0);
        let builder = egui::ViewportBuilder::default()
            .with_title("MailCraft: new mail")
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top()
            .with_taskbar(false)
            .with_active(false)
            .with_inner_size(size)
            .with_position(at);
        let icon = self.card_icon.clone();
        let mut click = false;
        let mut close = false;
        let mut hovered = false;
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("mc_new_mail_card"), builder, |ctx, _class| {
            // A solid window (a see-through one shows as nothing without a compositor); its
            // contents fade in and out.
            let frame = egui::Frame::NONE.fill(t.menu).stroke(egui::Stroke::new(1.0, t.border_strong)).inner_margin(egui::Margin::same(12));
            egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
                ui.set_opacity(fade);
                let whole = ui.max_rect();
                let resp = ui.interact(whole, ui.id().with("card"), Sense::click());
                hovered = resp.hovered();
                ui.horizontal_top(|ui| {
                    if let Some(icon) = &icon {
                        ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(40.0, 40.0)));
                    }
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&card.heading).font(regular(11.0)).color(t.text_dim));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if people::cross_button(ui, t).on_hover_text("Close").clicked() {
                                    close = true;
                                }
                            });
                        });
                        ui.add(egui::Label::new(RichText::new(&card.title).font(semibold(14.0)).color(t.text)).truncate());
                        for (k, l) in card.lines.iter().enumerate() {
                            let (font, colour) = if k == 0 { (semibold(12.5), t.text) } else { (regular(12.0), t.text_dim) };
                            ui.add(egui::Label::new(RichText::new(l).font(font).color(colour)).truncate());
                        }
                    });
                });
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    click = true;
                }
            });
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(if fade < 1.0 { 30 } else { 250 }));
        if hovered && let Some(c) = self.card.as_mut() {
            // Held while the mouse is on it: the time starts again (after the fade in).
            c.born = std::time::Instant::now() - std::time::Duration::from_secs_f32(CARD_FADE.min(age));
        }
        if click {
            // MailCraft to the front, at the message.
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
            if let Some(c) = self.card.take() {
                self.card_open = c.open;
                self.card_day = c.day;
            }
        } else if close {
            self.card = None;
        }
    }

    /// Open a message a notice was about: its folder, the mail tab, the message.
    fn open_notified(&mut self, (folder, uid): (String, u32)) {
        self.active_tab = 0;
        if self.folder.as_deref() != Some(folder.as_str()) || self.vfolder.is_some() {
            self.vfolder = None;
            self.folder = Some(folder.clone());
            self.show_other = false;
            self.refresh_messages();
        }
        self.open_message(folder, uid);
    }

    /// The banners, bottom left (clear of the browser view, which sits over everything).
    fn draw_banners(&mut self, ctx: &egui::Context, t: &Tokens) {
        let now = std::time::Instant::now();
        self.banners.retain(|b| b.sticky || now.duration_since(b.born).as_secs() < if b.error { 10 } else { 8 });
        if self.banners.is_empty() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        let mut close = None;
        let mut open = None;
        egui::Area::new(egui::Id::new("mc_banners")).order(egui::Order::Foreground).anchor(egui::Align2::LEFT_BOTTOM, vec2(12.0, -34.0)).show(
            ctx,
            |ui| {
                for (i, b) in self.banners.iter().enumerate() {
                    let frame = egui::Frame::popup(ui.style())
                        .fill(t.menu)
                        .stroke(egui::Stroke::new(if b.error { 1.5 } else { 1.0 }, if b.error { t.red } else { t.border_strong }))
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(12, 8));
                    let shown = frame.show(ui, |ui| {
                        ui.set_width(330.0);
                        ui.horizontal(|ui| {
                            let kind = if b.error {
                                "Error"
                            } else if b.open.is_some() || b.lines.len() > 1 || b.title.ends_with("new messages") {
                                "New mail"
                            } else {
                                "Done"
                            };
                            ui.label(RichText::new(kind).font(regular(11.0)).color(if b.error { t.red } else { t.text_dim }));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if people::cross_button(ui, t).on_hover_text("Dismiss").clicked() {
                                    close = Some(i);
                                }
                            });
                        });
                        ui.add(egui::Label::new(RichText::new(&b.title).font(semibold(13.0)).color(t.text)).truncate());
                        for l in &b.lines {
                            ui.add(egui::Label::new(RichText::new(l).font(regular(12.0)).color(t.text_dim)).truncate());
                        }
                    });
                    if b.open.is_some() && shown.response.interact(Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        open = b.open.clone();
                        close = Some(i);
                    }
                    ui.add_space(6.0);
                }
            },
        );
        if let Some(i) = close.filter(|i| *i < self.banners.len()) {
            self.banners.remove(i);
        }
        if let Some((folder, uid)) = open {
            if self.folder.as_deref() != Some(folder.as_str()) || self.vfolder.is_some() {
                self.vfolder = None;
                self.folder = Some(folder.clone());
                self.show_other = false;
                self.refresh_messages();
            }
            self.open_message(folder, uid);
        }
    }

    /// Save the search history as a text file, one search a line (Settings > Privacy and data).
    fn export_search_history(&mut self) {
        let Some(path) = rfd::FileDialog::new().set_file_name("MailCraft search history.txt").add_filter("Text", &["txt"]).save_file() else {
            return;
        };
        let text = self.settings.search_history.join("\r\n");
        self.status = match std::fs::write(&path, text) {
            Ok(()) => format!("Search history saved to {}.", path.display()),
            Err(e) => format!("Could not save the search history: {e}"),
        };
    }

    /// Automatic replies (Settings > Account): one to each sender of new mail in the Inbox since
    /// they were switched on, while MailCraft is running. Not to lists, newsletters or
    /// notifications, blocked senders, or (by choice) people never written to.
    fn run_auto_replies(&mut self) {
        let Some(acct) = self.current().cloned() else { return };
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let Some(ar) = self.settings.auto_replies.iter().find(|a| a.account == acct.email && a.active_on(&today)).cloned() else { return };
        let Some(inbox) = self.folders.iter().find(|f| foldertree::kind(f) == foldertree::Kind::Inbox).map(|f| f.name.clone()) else { return };
        let Some(store) = &self.store else { return };
        let Ok(Some(id)) = store.folder_id(&acct.email, &inbox) else { return };
        let after = ar.since.clone().max(ar.start.trim().to_string());
        let me = acct.email.to_ascii_lowercase();
        let mut answer: Vec<(String, MessageRow)> = vec![];
        for m in store.messages(id, 500).unwrap_or_default() {
            if m.date.as_deref().unwrap_or("") < after.as_str() {
                continue;
            }
            let from = m.from_addr.as_deref().unwrap_or("").trim().to_ascii_lowercase();
            let skip = from.is_empty()
                || from == me
                || m.bulk
                || settings::Settings::listed(&self.settings.blocked, &from)
                || (ar.known_only && !self.known.contains(&from))
                || self.settings.auto_replied.iter().any(|(a, s, k)| *a == acct.email && *s == from && *k == ar.since)
                || answer.iter().any(|(f, _)| *f == from);
            if !skip {
                answer.push((from, m));
            }
        }
        if answer.is_empty() {
            return;
        }
        let Some((acct, pw)) = self.password() else { return };
        for (from, m) in &answer {
            let subject = m.subject.clone().unwrap_or_default();
            let out = mailcraft_core::send::Outgoing {
                from: acct.email.clone(),
                from_name: self.settings.name_for(&acct.email),
                to: vec![from.clone()],
                subject: if ar.subject.trim().is_empty() { format!("Automatic reply: {subject}") } else { ar.subject.clone() },
                text: ar.text.clone(),
                in_reply_to: m.message_id.clone(),
                references: m.message_id.iter().cloned().collect(),
                auto_reply: true,
                ..Default::default()
            };
            let _ = self.worker.send(Request::SendOnly(acct.clone(), pw.clone(), Box::new(out)));
            self.settings.auto_replied.push((acct.email.clone(), from.clone(), ar.since.clone()));
        }
        let _ = settings::save(&self.settings);
        self.status = if answer.len() == 1 { "Sent 1 automatic reply.".into() } else { format!("Sent {} automatic replies.", answer.len()) };
        self.keep_status = true;
    }

    /// The current folder's row.
    fn folder_row(&self) -> Option<&FolderRow> {
        self.folder.as_ref().and_then(|n| self.folders.iter().find(|f| f.name == *n))
    }

    fn kind_of(&self, folder: &str) -> foldertree::Kind {
        self.folders.iter().find(|f| f.name == folder).map(foldertree::kind).unwrap_or(foldertree::Kind::Folder)
    }

    fn selected_row(&self) -> Option<&MessageRow> {
        let (f, u) = self.selected.as_ref()?;
        self.messages.iter().find(|m| m.uid == *u && m.folder == *f)
    }

    /// The whole message as it came from the server, from the cache (it is there once opened).
    fn selected_raw(&self) -> Option<Vec<u8>> {
        let (f, u) = self.selected.as_ref()?;
        let (store, acct) = (self.store.as_ref()?, self.current()?);
        let id = store.folder_id(&acct.email, f).ok()??;
        store.body(id, *u).ok()?
    }

    /// Whether a ribbon button / menu item can be used now, and whether it shows as on.
    fn state(&self, a: Action) -> ribbon::State {
        use foldertree::Kind;
        let on = |enabled: bool| ribbon::State { enabled, checked: false };
        let has = !self.accounts.is_empty();
        let idle = self.compose.is_none();
        let targets = self.targets();
        let sel = idle && !targets.is_empty();
        let one = self.picked.len() <= 1;
        let open = idle && one && self.open.is_some();
        let any_kind = |k: Kind| targets.iter().any(|(f, _)| self.kind_of(f) == k);
        let all_kind = |k: Kind| !targets.is_empty() && targets.iter().all(|(f, _)| self.kind_of(f) == k);
        let here = self.folder_row().map(foldertree::kind);
        match a {
            Action::AddAccount
            | Action::Exit
            | Action::Shortcuts
            | Action::About
            | Action::OpenLogs
            | Action::ExportSettings
            | Action::ImportSettings => on(true),
            Action::RemoveAccount => on(has && idle),
            Action::SaveAs | Action::ViewSource => on(sel && one && self.selected_raw().is_some()),
            Action::Print => on(open && self.can_print()),
            Action::NewMail => on(has && idle),
            Action::Reply | Action::ReplyAll | Action::Forward | Action::ForwardAttachment => on(open),
            Action::EditDraft => on(open && self.open.as_ref().is_some_and(|o| self.is_drafts(&o.folder))),
            Action::Delete | Action::MoveTo | Action::CopyTo | Action::ToggleRead => on(sel),
            Action::Restore => on(sel && self.trash_name().is_some_and(|t| targets.iter().all(|(f, _)| *f == t))),
            // Always: with nothing to undo it says so, rather than nothing happening.
            Action::Undo => on(idle),
            Action::RefreshFolder => on(has && idle && (here.is_some() || self.vfolder.is_some())),
            Action::GoBack => on(idle && self.history.can_back(|v| self.visit_exists(v))),
            Action::GoForward => on(idle && self.history.can_forward(|v| self.visit_exists(v))),
            Action::Rules => on(has && idle),
            Action::Categorise => on(sel),
            Action::ToggleFavourite => ribbon::State {
                enabled: idle && here.is_some(),
                checked: self
                    .folder
                    .as_deref()
                    .is_some_and(|f| self.folder_prefs.is_favourite(self.current().map(|a| a.email.as_str()).unwrap_or(""), f)),
            },
            Action::FolderColour(_) => on(here.is_some()),
            Action::CreateRule => on(sel && one && self.selected_row().is_some()),
            Action::RunRules => {
                let mine = self.current().map(|a| a.email.as_str()).unwrap_or("");
                on(idle && here.is_some() && self.rules.iter().any(|r| r.enabled && r.account == mine))
            }
            Action::Archive => on(sel && !all_kind(Kind::Archive)),
            Action::Junk => on(sel && !all_kind(Kind::Junk)),
            Action::NotJunk => on(sel && any_kind(Kind::Junk)),
            Action::ToggleFlag => ribbon::State {
                enabled: sel,
                checked: sel && targets.iter().all(|(f, u)| self.messages.iter().any(|m| m.uid == *u && m.folder == *f && flagged(m))),
            },
            Action::FocusSearch => on(has),
            Action::Filter(f) => ribbon::State { enabled: has, checked: self.filter == f },
            Action::SendReceive | Action::SendReceiveAll => on(has && !self.syncing),
            Action::NewFolder => on(has && idle),
            Action::NewSubfolder | Action::MarkAllRead => on(idle && here.is_some()),
            Action::RenameFolder | Action::DeleteFolder => on(idle && here == Some(Kind::Folder)),
            // Any folder: Trash and Junk are emptied for good, the rest into Trash.
            Action::EmptyFolder => on(idle && here.is_some()),
            Action::Pane(p) => ribbon::State { enabled: true, checked: self.pane_at == p },
            Action::ToggleFolderPane => ribbon::State { enabled: true, checked: self.show_folders },
            Action::ToggleRibbon => ribbon::State { enabled: true, checked: self.fold.collapsed },
            Action::Conversations => ribbon::State { enabled: true, checked: self.threads_on() },
            Action::Settings => on(true),
            Action::BlockSender => on(sel && one),
            Action::AlwaysFocused => on(sel && one && self.focused_here() && self.show_other),
            Action::AlwaysOther => on(sel && one && self.focused_here() && !self.show_other),
            Action::QuickStep(_) => on(sel),
            Action::ManageQuickSteps => on(has),
            Action::Sweep => on(sel && one && self.selected_row().is_some_and(|m| m.from_addr.is_some())),
            Action::Theme(th) => ribbon::State { enabled: true, checked: self.theme == th },
            Action::DarkMessages => ribbon::State { enabled: self.applied == Some(looks::Theme::Dark), checked: self.dark_messages },
            Action::SimplifiedRibbon => ribbon::State { enabled: true, checked: self.fold.simplified },
            Action::Sort(k) => ribbon::State { enabled: true, checked: self.sort == k },
            Action::SortDirection => ribbon::State { enabled: true, checked: self.reverse },
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn can_print(&self) -> bool {
        self.web.is_some()
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    fn can_print(&self) -> bool {
        false
    }

    /// Do what a ribbon button, menu item or shortcut asks.
    fn act(&mut self, a: Action, ctx: &egui::Context) {
        if !self.state(a).enabled {
            return;
        }
        use mailcraft_core::manage::Special;
        match a {
            Action::AddAccount => self.add = Some(AddDialog::default()),
            Action::ExportSettings => self.export_settings(),
            Action::ImportSettings => self.import_settings(),
            Action::RemoveAccount => self.dialog = Some(Dialog::RemoveAccount(self.account)),
            Action::SaveAs => {
                if let Some(raw) = self.selected_raw() {
                    let subject = self.selected_row().and_then(|m| m.subject.clone()).unwrap_or_else(|| "message".into());
                    let name = format!("{}.eml", safe_file_name(&subject));
                    if let Some(path) =
                        rfd::FileDialog::new().set_title("Save message").set_file_name(name).add_filter("Email message", &["eml"]).save_file()
                    {
                        self.status = match std::fs::write(&path, raw) {
                            Ok(()) => format!("Saved {}.", path.display()),
                            Err(e) => format!("Could not save: {e}"),
                        };
                    }
                }
            }
            Action::Print => self.print(),
            Action::ViewSource => {
                if let Some(raw) = self.selected_raw() {
                    let title = self.selected_row().and_then(|m| m.subject.clone()).unwrap_or_default();
                    let mut text = String::from_utf8_lossy(&raw).into_owned();
                    if text.len() > 2_000_000 {
                        let mut cut = 2_000_000;
                        while !text.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        text.truncate(cut);
                        text.push_str("\n\n[... cut: the rest is too long to show here; use Save as]");
                    }
                    self.dialog = Some(Dialog::Source { title, text });
                }
            }
            Action::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::NewMail => self.start_compose("new"),
            Action::Reply => self.start_compose("reply"),
            Action::ReplyAll => self.start_compose("all"),
            Action::Forward => self.start_compose("forward"),
            Action::ForwardAttachment => self.start_compose("attach"),
            Action::EditDraft => self.start_compose("draft"),
            Action::Delete => self.delete_selected(),
            Action::Restore => self.restore_selected(),
            Action::Undo => self.undo_delete(),
            Action::RefreshFolder => self.refresh_folder(),
            Action::GoBack => self.go_back_or_forward(false),
            Action::GoForward => self.go_back_or_forward(true),
            Action::Archive => self.move_special(Special::Archive),
            Action::Junk => self.move_special(Special::Junk),
            Action::NotJunk => self.move_special(Special::Inbox),
            Action::MoveTo => self.moving = Some((self.targets(), false)),
            Action::CopyTo => self.moving = Some((self.targets(), true)),
            Action::ToggleRead => self.toggle_read(),
            Action::Rules => self.dialog = Some(Dialog::Rules),
            Action::Categorise => self.dialog = Some(Dialog::Categorise),
            Action::ToggleFavourite => {
                if let (Some(acct), Some(f)) = (self.current().map(|a| a.email.clone()), self.folder.clone()) {
                    if self.folder_prefs.is_favourite(&acct, &f) {
                        self.folder_prefs.favourites.retain(|r| !(r.account == acct && r.folder == f));
                    } else {
                        self.folder_prefs.favourites.push(accounts::FolderRef { account: acct, folder: f });
                    }
                    self.save_folder_prefs();
                }
            }
            Action::FolderColour(c) => {
                if let (Some(acct), Some(f)) = (self.current().map(|a| a.email.clone()), self.folder.clone()) {
                    self.folder_prefs.colours.retain(|(r, _)| !(r.account == acct && r.folder == f));
                    if let Some(c) = c {
                        self.folder_prefs.colours.push((accounts::FolderRef { account: acct, folder: f }, c));
                    }
                    self.save_folder_prefs();
                }
            }
            Action::CreateRule => {
                if let Some(m) = self.selected_row() {
                    let from = m.from_addr.clone().or(m.from_name.clone()).unwrap_or_default();
                    let who = m.from_name.clone().or(m.from_addr.clone()).unwrap_or_default();
                    let account = self.current().map(|a| a.email.clone()).unwrap_or_default();
                    let rule = mailcraft_core::rules::Rule { name: format!("From {who}"), from, account, ..Default::default() };
                    self.dialog = Some(Dialog::RuleEdit { index: None, rule });
                }
            }
            Action::RunRules => {
                if let (Some(f), Some((acct, pw))) = (self.folder.clone(), self.password()) {
                    self.status = format!("Running the rules on {f}...");
                    let _ = self.worker.send(Request::RunRules(acct, pw, f));
                }
            }
            Action::ToggleFlag => self.toggle_flag(),
            Action::FocusSearch => self.focus_search = true,
            Action::Filter(f) => {
                self.filter = f;
                self.refresh_messages();
            }
            Action::SendReceive => self.sync(self.account),
            Action::SendReceiveAll => {
                for i in 0..self.accounts.len() {
                    self.sync(i);
                }
            }
            Action::NewFolder => self.dialog = Some(Dialog::NewFolder { parent: None, name: String::new() }),
            Action::NewSubfolder => self.dialog = Some(Dialog::NewFolder { parent: self.folder.clone(), name: String::new() }),
            Action::RenameFolder => {
                if let Some(f) = self.folder_row() {
                    let name = f.delimiter.as_deref().filter(|d| !d.is_empty()).and_then(|d| f.name.rsplit(d).next()).unwrap_or(&f.name).to_string();
                    self.dialog = Some(Dialog::Rename { from: f.name.clone(), name });
                }
            }
            Action::DeleteFolder => self.dialog = self.folder.clone().map(Dialog::DeleteFolder),
            Action::MarkAllRead => {
                if let Some(f) = self.folder.clone() {
                    self.folder_op(FolderOp::MarkAllRead(f));
                }
            }
            Action::EmptyFolder => self.dialog = self.folder.clone().map(Dialog::Empty),
            Action::Pane(p) => self.pane_at = p,
            Action::ToggleFolderPane => self.show_folders = !self.show_folders,
            Action::ToggleRibbon => self.fold.toggle(),
            Action::Conversations => self.dialog = Some(Dialog::Conversations { on: !self.threads_on() }),
            Action::Settings => {
                self.quota = None;
                self.open_settings(settings_ui::Page::MessageList);
            }
            Action::QuickStep(i) => self.run_quick_step(i as usize),
            Action::ManageQuickSteps => {
                self.set_sel = None;
                self.open_settings(settings_ui::Page::QuickSteps);
            }
            Action::Sweep => {
                if let Some(m) = self.selected_row() {
                    let sender = m.from_addr.clone().unwrap_or_default();
                    let folder = m.folder.clone();
                    self.dialog = Some(Dialog::Sweep { sender, folder, choice: 0, days: 10, move_to: None });
                }
            }
            Action::BlockSender => {
                if let Some(addr) = self.selected_row().and_then(|m| m.from_addr.clone()).filter(|a| !a.is_empty()) {
                    if !settings::Settings::listed(&self.settings.blocked, &addr) {
                        self.settings.blocked.push(addr.clone());
                        let _ = settings::save(&self.settings);
                    }
                    self.status = format!("{addr} is blocked: their mail goes to Junk.");
                    self.move_special(Special::Junk);
                }
            }
            Action::AlwaysFocused | Action::AlwaysOther => {
                if let Some(addr) = self.selected_row().and_then(|m| m.from_addr.clone()).filter(|a| !a.is_empty()) {
                    let other = a == Action::AlwaysOther;
                    let addr = addr.to_ascii_lowercase();
                    self.settings.focused_senders.retain(|x| !x.eq_ignore_ascii_case(&addr));
                    self.settings.other_senders.retain(|x| !x.eq_ignore_ascii_case(&addr));
                    if other {
                        self.settings.other_senders.push(addr.clone());
                    } else {
                        self.settings.focused_senders.push(addr.clone());
                    }
                    let _ = settings::save(&self.settings);
                    self.status = format!("Mail from {addr} will go to {}.", if other { "Other" } else { "Focused" });
                    self.selected = None;
                    self.open = None;
                    self.refresh_messages();
                }
            }
            Action::Theme(th) => self.theme = th,
            Action::DarkMessages => {
                self.dark_messages = !self.dark_messages;
                self.restyle_message();
            }
            Action::SimplifiedRibbon => self.fold.simplified = !self.fold.simplified,
            Action::Sort(k) => {
                self.sort = k;
                self.refresh_messages();
            }
            Action::SortDirection => {
                self.reverse = !self.reverse;
                self.refresh_messages();
            }
            Action::Shortcuts => self.dialog = Some(Dialog::Shortcuts),
            Action::About => self.dialog = Some(Dialog::About),
            Action::OpenLogs => {
                let dir = logfile::dir();
                let _ = std::fs::create_dir_all(&dir);
                if let Err(e) = open::that_detached(&dir) {
                    self.status = format!("The log is in {} ({e}).", dir.display());
                }
            }
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn print(&mut self) {
        if let Some(w) = self.web.as_ref()
            && let Err(e) = w.print()
        {
            self.status = format!("Could not print: {e}");
        }
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    fn print(&mut self) {}

    fn folder_op(&mut self, op: FolderOp) {
        if let Some(acct) = self.current().map(|a| a.email.clone()) {
            let delim = self.delimiter();
            match &op {
                FolderOp::Rename(from, to) => {
                    self.folder_prefs.renamed(&acct, from, to, &delim);
                    self.save_folder_prefs();
                }
                FolderOp::Delete(name) => {
                    self.folder_prefs.forget(&acct, Some(name), &delim);
                    self.save_folder_prefs();
                }
                _ => {}
            }
        }
        let Some(acct) = self.current().cloned() else { return };
        let Some(pw) = secrets::get(&acct.email) else {
            self.prompt = Some((self.account, String::new()));
            return;
        };
        self.status = "Working...".into();
        self.folder_op_here(&acct.email, &op);
        let _ = self.worker.send(Request::Folder(acct, pw, op));
    }

    /// Emptying a folder or marking it read changes this PC's copy and the screen first; the
    /// server follows in the background (2026-10-29: emptying Trash took 8.5 s on the
    /// server and the window waited for it).
    pub(crate) fn folder_op_here(&mut self, email: &str, op: &FolderOp) {
        let cleared = {
            let Some(store) = &self.store else { return };
            let id = |name: &str| store.folder_id(email, name).ok().flatten();
            let mine: Vec<FolderRow> = self.folders.iter().filter(|f| f.account == email).cloned().collect();
            let trash = mailcraft_core::manage::find_trash(&mine);
            let name = match op {
                FolderOp::MarkAllRead(n) => {
                    if let Some(i) = id(n) {
                        let _ = store.mark_all_read(i);
                    }
                    None
                }
                FolderOp::Empty(n) | FolderOp::EmptyToTrash(n) => Some(n.clone()),
                FolderOp::EmptyTrash => trash.clone(),
                _ => return,
            };
            if let Some(i) = name.as_deref().and_then(id) {
                // Restore's records: everything emptied into Trash is remembered; Trash emptied
                // for good, forgotten.
                let r = match op {
                    FolderOp::EmptyToTrash(_) => store.remember_folder_origin(i),
                    _ if name == trash => store.forget_origins_in(i, None),
                    _ => Ok(0),
                };
                if let Err(e) = r {
                    log::warn!("{email}: keeping where Trash came from: {e}");
                }
                let _ = store.clear_folder(i);
            }
            name
        };
        if let Some(n) = cleared
            && self.selected.as_ref().is_some_and(|(f, _)| *f == n)
        {
            self.selected = None;
            self.open = None;
        }
        self.refresh_keeping_place();
    }

    fn move_special(&mut self, which: mailcraft_core::manage::Special) {
        let targets = self.targets();
        self.move_special_items(targets, which);
    }

    fn move_special_items(&mut self, items: Vec<(String, u32)>, which: mailcraft_core::manage::Special) {
        use foldertree::Kind;
        use mailcraft_core::manage::Special;
        let skip = match which {
            Special::Archive => Kind::Archive,
            Special::Junk => Kind::Junk,
            Special::Inbox => Kind::Inbox,
        };
        let targets: Vec<(String, u32)> = items.into_iter().filter(|(f, _)| self.kind_of(f) != skip).collect();
        let Some((acct, pw)) = self.password().filter(|_| !targets.is_empty()) else { return };
        for (folder, uids) in by_folder(&targets) {
            let _ = self.worker.send(Request::MoveSpecial(acct.clone(), pw.clone(), folder, uids, which));
        }
        self.status = "Moving...".into();
        self.take_out(&targets);
    }

    /// Flag: if any of them is not flagged, flag them all; otherwise take the flags off.
    fn toggle_flag(&mut self) {
        let targets = self.targets();
        self.toggle_flag_items(&targets);
    }

    fn toggle_flag_items(&mut self, targets: &[(String, u32)]) {
        let on = targets.iter().any(|(f, u)| !self.messages.iter().chain(&self.extra).any(|m| m.uid == *u && m.folder == *f && flagged(m)));
        self.set_flag_on(targets, "\\Flagged", on);
    }

    /// The account's quick steps, in order.
    fn my_quick_steps(&self) -> Vec<settings::QuickStep> {
        let mine = self.current().map(|a| a.email.as_str()).unwrap_or("");
        self.settings.quick_steps.iter().filter(|q| q.account == mine).cloned().collect()
    }

    /// A quick step on the selected messages: flags and category first, then move or delete.
    fn run_quick_step(&mut self, i: usize) {
        let Some(q) = self.my_quick_steps().into_iter().nth(i) else { return };
        let items = self.targets();
        if items.is_empty() {
            return;
        }
        if q.mark_read {
            self.set_flag_on(&items, "\\Seen", true);
        }
        if q.flag {
            self.set_flag_on(&items, "\\Flagged", true);
        }
        if let Some(key) = q.categorise.clone()
            && let Some((acct, pw)) = self.password()
        {
            let _ = self.worker.send(Request::Categorise(acct, pw, by_folder(&items), key, true));
        }
        if q.delete {
            self.delete_items(items);
        } else if let Some(to) = q.move_to.clone() {
            self.move_to(items, to, false);
        }
        self.status = format!("Quick step: {}.", q.name);
    }

    /// Folders of the open account as (name, indented label), for choosing one.
    fn folder_choices(&self) -> Vec<(String, String)> {
        foldertree::rows(&self.folders, &Default::default())
            .into_iter()
            .filter_map(|r| r.folder.and_then(|i| self.folders.get(i)).map(|f| (f.name.clone(), format!("{}{}", "    ".repeat(r.depth), r.label))))
            .collect()
    }

    /// The messages a sweep takes in a folder: from `sender`, minus the ones it keeps.
    fn sweep_victims(&self, rows: &[MessageRow], sender: &str, keep_latest: bool, keep_days: Option<u32>) -> Vec<(String, u32)> {
        let mut from: Vec<&MessageRow> = rows.iter().filter(|m| m.from_addr.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(sender))).collect();
        from.sort_by(|a, b| b.date.cmp(&a.date));
        let cutoff = keep_days.map(|d| (chrono::Utc::now() - chrono::Duration::days(d as i64)).to_rfc3339());
        from.into_iter()
            .enumerate()
            .filter(|(i, m)| {
                if keep_latest {
                    *i > 0
                } else if let Some(c) = &cutoff {
                    m.date.as_deref().is_some_and(|d| d < c.as_str())
                } else {
                    true
                }
            })
            .map(|(_, m)| (m.folder.clone(), m.uid))
            .collect()
    }

    /// Standing sweeps for the open account, after a sync.
    fn run_sweeps(&mut self) {
        let Some(acct) = self.current().map(|a| a.email.clone()) else { return };
        let sweeps: Vec<settings::Sweep> = self.settings.sweeps.iter().filter(|w| w.account == acct).cloned().collect();
        for w in sweeps {
            let rows = match &self.store {
                Some(st) => st.folder_id(&acct, &w.folder).ok().flatten().and_then(|id| st.messages(id, 5000).ok()).unwrap_or_default(),
                None => continue,
            };
            let victims = self.sweep_victims(&rows, &w.sender, w.keep_latest, w.older_than_days);
            if victims.is_empty() {
                continue;
            }
            match w.move_to.clone() {
                Some(to) => self.move_to(victims, to, false),
                None => self.delete_items(victims),
            }
        }
    }

    /// Shift+Up/Down: grow the selection by the next line that way.
    fn pick_extend(&mut self, by: i32) {
        let mut set = self.targets();
        let picked_lines: Vec<usize> =
            self.lines.iter().enumerate().filter(|(_, l)| self.line_items(l).iter().any(|x| set.contains(x))).map(|(i, _)| i).collect();
        let mut next = if by > 0 { picked_lines.iter().max().map(|i| i + 1) } else { picked_lines.iter().min().and_then(|i| i.checked_sub(1)) };
        while let Some(n) = next
            && matches!(self.lines.get(n), Some(Line::Day(_)))
        {
            next = if by > 0 { Some(n + 1) } else { n.checked_sub(1) };
        }
        if let Some(l) = next.and_then(|n| self.lines.get(n)) {
            for x in self.line_items(l) {
                if !set.contains(&x) {
                    set.push(x);
                }
            }
            self.head = None;
            self.set_picked(set, None);
        }
    }

    fn select_all(&mut self) {
        let set: Vec<(String, u32)> = self.messages.iter().map(|m| (m.folder.clone(), m.uid)).collect();
        self.set_picked(set, None);
    }

    fn set_picked(&mut self, set: Vec<(String, u32)>, focus: Option<(String, u32)>) {
        match set.len() {
            0 => {
                self.picked.clear();
                self.selected = None;
                self.open = None;
            }
            1 => {
                self.picked.clear();
                if let Some((f, u)) = set.into_iter().next()
                    && self.selected != Some((f.clone(), u))
                {
                    self.open_message(f, u);
                }
            }
            n => {
                self.picked = set;
                if let Some(f) = focus.filter(|f| self.picked.contains(f)) {
                    self.selected = Some(f);
                }
                self.status = format!("{n} messages selected.");
            }
        }
    }

    fn remove_account(&mut self, idx: usize) {
        let Some(acct) = self.accounts.get(idx).cloned() else { return };
        secrets::forget(&acct.email);
        self.folder_prefs.forget(&acct.email, None, "/");
        let _ = accounts::save_folder_prefs(&self.folder_prefs);
        if let Some(store) = &self.store {
            let _ = store.prune_folders(&acct.email, &[]);
        }
        self.accounts.remove(idx);
        let _ = accounts::save(&self.accounts);
        self.account = 0;
        self.folder = None;
        self.selected = None;
        self.open = None;
        self.folders.clear();
        self.messages.clear();
        self.refresh_folders();
        self.status = format!("{} removed from MailCraft (the mail is still on the server).", acct.email);
    }

    /// Is this the account's Drafts folder? (Marked \\Drafts, or named like it when nothing is marked.)
    fn is_drafts(&self, folder: &str) -> bool {
        if let Some(f) = self.folders.iter().find(|f| f.special.as_deref() == Some("\\Drafts")) {
            return f.name == folder;
        }
        ["Drafts", "Draft", "INBOX.Drafts", "INBOX/Drafts"].iter().any(|g| g.eq_ignore_ascii_case(folder))
    }

    fn sync(&mut self, idx: usize) {
        let Some(acct) = self.accounts.get(idx).cloned() else { return };
        match secrets::get(&acct.email) {
            Some(pw) => {
                self.syncing = true;
                self.status = format!("Syncing {}...", acct.email);
                // Where each folder ends now, so what the sync brings can be announced.
                if let Some(marks) = self.store.as_ref().and_then(|st| st.max_uids(&acct.email).ok()) {
                    self.new_mail_marks.insert(acct.email.clone(), marks);
                }
                let _ = self.worker.send(Request::Sync(acct, pw));
            }
            None => self.prompt = Some((idx, String::new())),
        }
    }

    fn open_message(&mut self, folder: String, uid: u32) {
        self.picked.clear();
        self.head = None;
        self.selected = Some((folder.clone(), uid));
        let Some(acct) = self.current().cloned() else { return };
        self.open_asked = Some(((folder.clone(), uid), std::time::Instant::now()));
        // On this PC already (nearly everything is, once downloaded): shown now, never queued
        // behind whatever the server is being told (2026-10-29: "all the emails are
        // cached on the PC, so there should never be pauses").
        let cached = self.store.as_ref().and_then(|st| {
            let id = st.folder_id(&acct.email, &folder).ok()??;
            st.body(id, uid).ok()?
        });
        if let Some(raw) = cached {
            self.loading = None;
            self.on_opened(acct.email, folder, uid, Ok(raw), true);
            return;
        }
        let Some(pw) = secrets::get(&acct.email) else {
            self.prompt = Some((self.account, String::new()));
            return;
        };
        self.loading = Some(uid);
        let _ = self.worker.send(Request::Open(acct, pw, folder, uid));
    }

    fn show_message(&mut self, folder: String, uid: u32, raw: &[u8]) {
        let Some(m) = mail_parser::MessageParser::default().parse(raw) else {
            self.status = "That message could not be read.".into();
            return;
        };
        // Real HTML when the sender wrote it; plain text kept as plain text, not reflowed.
        let html = if m.html_body.iter().any(|&i| m.parts.get(i as usize).is_some_and(|p| p.is_text_html())) {
            m.body_html(0).map(|h| h.to_string()).unwrap_or_default()
        } else {
            htmlview::from_text(&m.body_text(0).unwrap_or_default())
        };
        let inline: Vec<htmlview::Inline> = m
            .parts
            .iter()
            .filter_map(|p| {
                let cid = p.content_id()?.to_string();
                let ct = p.content_type()?;
                Some(htmlview::Inline {
                    cid,
                    mime: format!("{}/{}", ct.ctype(), ct.subtype().unwrap_or("octet-stream")),
                    bytes: p.contents().to_vec(),
                })
            })
            .collect();
        let remote = htmlview::has_remote_images(&html);
        // Pictures from the internet: Settings > Reading pane (always, safe senders, or ask).
        let sender = m.from().and_then(|a| a.first()).and_then(|a| a.address()).unwrap_or("").to_string();
        let pictures_ok = match self.settings.pictures {
            settings::Pictures::Always => true,
            settings::Pictures::SafeSenders => settings::Settings::listed(&self.settings.safe, &sender),
            settings::Pictures::Ask => false,
        };
        let (thread, missing) = self.thread_cards(&folder, uid);
        let prepared = htmlview::prepare(&reading_html(&format!("{html}{thread}"), self.dark_reading()), &inline, pictures_ok, self.message_css());
        // Fetch what the cache does not have yet; the cards fill in as each arrives.
        if !missing.is_empty()
            && let Some(acct) = self.current().cloned()
            && let Some(pw) = secrets::get(&acct.email)
        {
            for (f, u) in missing {
                let _ = self.worker.send(Request::Open(acct.clone(), pw.clone(), f, u));
            }
        }
        // A read receipt asked for: the address in Disposition-Notification-To.
        let receipt_to = m.header_raw("Disposition-Notification-To").map(|v| {
            let v = v.trim();
            match (v.rfind('<'), v.rfind('>')) {
                (Some(a), Some(b)) if a < b => v.get(a + 1..b).unwrap_or(v).trim().to_string(),
                _ => v.to_string(),
            }
        });
        let mut doc = wordcraft_formats::html::import(&html);
        doc.ensure_nonempty();
        doc.settings.protection = Some("readOnly".into());
        let mut session = Session::new(doc);
        session.view.mode = wordcraft_layout::ViewMode::Web;
        session.view.ruler = false;
        session.view.proofing = false;
        let mut reader = WordApp::new(session, Services::default());
        reader.manage_theme = false;
        reader.ui.ribbon_collapsed = true;
        let list = |l: Option<&mail_parser::Address>| {
            l.map(|l| {
                l.iter()
                    .map(|a| match (a.name(), a.address()) {
                        (Some(n), Some(e)) => format!("{n} <{e}>"),
                        (None, Some(e)) => e.to_string(),
                        (Some(n), None) => n.to_string(),
                        _ => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
        };
        let addrs = |l: Option<&mail_parser::Address>| {
            l.map(|l| l.iter().filter_map(|a| a.address().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default()
        };
        let mut references: Vec<String> = match m.references() {
            mail_parser::HeaderValue::TextList(l) => l.iter().map(|s| s.to_string()).collect(),
            mail_parser::HeaderValue::Text(t) => vec![t.to_string()],
            _ => vec![],
        };
        let message_id = m.message_id().map(str::to_string);
        let in_reply_to = match m.in_reply_to() {
            mail_parser::HeaderValue::Text(t) => Some(t.to_string()),
            mail_parser::HeaderValue::TextList(l) => l.first().map(|s| s.to_string()),
            _ => None,
        };
        if let Some(id) = &message_id {
            references.push(id.clone());
        }
        let reply_to = if m.reply_to().is_some() { addrs(m.reply_to()) } else { addrs(m.from()) };
        let invite = invite::find(&m).map(|(method, e)| self.invite_for(method, e));
        self.open = Some(OpenMessage {
            folder,
            uid,
            subject: m.subject().unwrap_or("(no subject)").to_string(),
            from: list(m.from()),
            to: list(m.to()),
            date: m.date().map(|d| d.to_rfc822()).unwrap_or_default(),
            attachments: m
                .attachments()
                // Pictures the message body shows are part of it, not attachments (as in Outlook).
                .filter(|a| !a.content_id().is_some_and(|id| html.contains(&format!("cid:{}", id.trim_matches(['<', '>'])))))
                // A meeting invitation's own calendar part is shown as the invitation card.
                .filter(|a| {
                    let cal = a
                        .content_type()
                        .is_some_and(|c| c.ctype().eq_ignore_ascii_case("text") && c.subtype().is_some_and(|s| s.eq_ignore_ascii_case("calendar")));
                    !(cal && a.attachment_name().is_none())
                })
                .map(|a| {
                    let mime = a
                        .content_type()
                        .map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")))
                        .unwrap_or_else(|| "application/octet-stream".into());
                    (a.attachment_name().unwrap_or("attachment").to_string(), mime, a.contents().to_vec())
                })
                .collect(),
            reader,
            message_id,
            in_reply_to,
            references,
            reply_to,
            to_list: addrs(m.to()),
            cc_list: addrs(m.cc()),
            html,
            inline,
            remote,
            show_remote: pictures_ok,
            receipt_to: receipt_to.filter(|a| a.contains('@')),
            prepared,
            thread,
            invite,
        });
        if self.pop_pending.as_ref().is_some_and(|(f, u, _)| self.open.as_ref().is_some_and(|o| o.folder == *f && o.uid == *u)) {
            let at = self.pop_pending.take().and_then(|p| p.2);
            if let Some(msg) = self.open.take() {
                self.add_popout(msg, at);
            }
        }
    }

    /// The other messages of the selected conversation, newest first, as cards to show under
    /// the open one; and those whose text is not in the cache yet.
    fn thread_cards(&self, folder: &str, uid: u32) -> (String, Vec<(String, u32)>) {
        let mut missing = vec![];
        if !self.settings.conversation_reading {
            return (String::new(), missing);
        }
        let (Some(head), Some(store), Some(acct)) = (self.head.as_ref(), &self.store, self.current()) else { return (String::new(), missing) };
        if !head.iter().any(|(f, u)| f == folder && *u == uid) {
            return (String::new(), missing);
        }
        let mut cards = String::new();
        let mut n = 0;
        for (f, u) in head.iter().filter(|(f, u)| !(f == folder && *u == uid)) {
            let Some(row) = self.messages.iter().chain(self.extra.iter()).find(|m| m.uid == *u && m.folder == *f) else { continue };
            n += 1;
            let who = escape_html(&row.from_name.clone().or(row.from_addr.clone()).unwrap_or_default());
            let when = escape_html(&short_date(&row.date));
            let place = if *f == folder { String::new() } else { format!(" &middot; {}", escape_html(&folder_label(f, &self.folders))) };
            let raw = store.folder_id(&acct.email, f).ok().flatten().and_then(|id| store.body(id, *u).ok().flatten());
            let text = match raw.as_deref().and_then(|r| mail_parser::MessageParser::default().parse(r)) {
                Some(m) => escape_html(&without_quote(&m.body_text(0).unwrap_or_default())),
                None => {
                    missing.push((f.clone(), *u));
                    "<i>Loading...</i>".to_string()
                }
            };
            cards.push_str(&format!(
                "<div style=\"margin-top:14px;padding-top:12px;border-top:1px solid #d6d6d6\">\
                 <div style=\"font-weight:600\">{who}</div><div style=\"color:#6b6b6b;font-size:12px\">{when}{place}</div>\
                 <div style=\"margin-top:8px;white-space:pre-wrap\">{text}</div></div>"
            ));
        }
        if n == 0 {
            return (String::new(), missing);
        }
        (
            format!(
                "<div style=\"margin:30px 18px 18px;font-family:'Segoe UI',Arial,sans-serif;font-size:14px;color:#242424\">\
                 <div style=\"color:#6b6b6b;font-size:12px;letter-spacing:.04em\">EARLIER IN THIS CONVERSATION ({n})</div>{cards}</div>"
            ),
            missing,
        )
    }

    /// A message of the open conversation arrived: show it among the cards.
    fn refresh_thread(&mut self) {
        let Some((folder, uid)) = self.open.as_ref().map(|o| (o.folder.clone(), o.uid)) else { return };
        let (thread, _) = self.thread_cards(&folder, uid);
        if let Some(o) = self.open.as_mut()
            && o.thread != thread
        {
            o.thread = thread;
            self.restyle_message();
        }
    }

    fn drain(&mut self) {
        while let Ok(ev) = self.worker.rx.try_recv() {
            match ev {
                Event::Discovered(email, r) => {
                    // The send-only address form asked: fill in its outgoing server.
                    if let Some(f) = self.sender_form.as_mut().filter(|f| f.busy && f.email.trim() == email) {
                        f.busy = false;
                        match r.as_ref().map(|s| s.smtp().cloned()) {
                            Ok(Some(smtp)) => {
                                f.host = smtp.host;
                                f.port = smtp.port;
                                f.security = smtp.security;
                                f.username = smtp.username;
                                // Outlook.com and Gmail: their own sign-in, not a password.
                                f.oauth = accounts::provider_for(&f.email, &f.host);
                                f.result = Some(Ok("Found the outgoing server.".into()));
                            }
                            Ok(None) => f.result = Some(Err("Found settings, but no encrypted outgoing server.".into())),
                            Err(e) => f.result = Some(Err(e.clone())),
                        }
                    }
                    if let Some(d) = self.add.as_mut().filter(|d| d.email.trim() == email) {
                        d.busy = false;
                        match r {
                            Ok(s) => {
                                d.error =
                                    if s.imap().is_none() { Some("Found settings, but no encrypted IMAP server among them.".into()) } else { None };
                                d.found = Some(s);
                            }
                            Err(e) => d.error = Some(e),
                        }
                    }
                }
                Event::SignedIn(email, r) => self.signed_in(email, r),
                Event::AccountSmtpTested(email, r) => {
                    if let Some(f) = self.account_form.as_mut().filter(|f| f.email.eq_ignore_ascii_case(&email)) {
                        f.busy = false;
                        f.result = Some(r.map(|_| "Signed in to the outgoing server: sending will work.".to_string()));
                    }
                }
                Event::ImapTested(email, r) => {
                    if let Some(f) = self.account_form.as_mut().filter(|f| f.email.eq_ignore_ascii_case(&email)) {
                        f.busy = false;
                        f.result = Some(r.map(|_| "Signed in to the incoming server: it works.".to_string()));
                    }
                }
                Event::SmtpTested(email, r) => {
                    if let Some(f) = self.sender_form.as_mut().filter(|f| f.email.trim().eq_ignore_ascii_case(&email)) {
                        f.busy = false;
                        f.result = Some(r.map(|_| "Signed in to the server: it works.".to_string()));
                    }
                }
                Event::Categorised(note) => {
                    if let Some(n) = note {
                        self.status = n;
                    }
                    self.refresh_messages();
                }
                Event::Done(r) => {
                    if let Some((left, _)) = self.exiting.as_mut() {
                        *left = left.saturating_sub(1);
                        continue;
                    }
                    let failed = r.is_err();
                    self.status = match r {
                        Ok(m) => m,
                        Err(e) => format!("That did not work: {e}"),
                    };
                    let msg = self.status.clone();
                    self.notice(&msg, failed);
                    // The sync that follows should not wipe what was just done off the status bar.
                    self.keep_status = true;
                    if !self.syncing {
                        self.sync(self.account);
                    }
                }
                Event::FolderRefreshed(email, folder, r) => self.folder_refreshed(email, folder, r),
                Event::Moved(r) => {
                    // Emptying Trash on the way out comes back here (a messages-only folder
                    // change), and closing waits for each account's.
                    if let Some((left, _)) = self.exiting.as_mut() {
                        *left = left.saturating_sub(1);
                        continue;
                    }
                    let failed = r.is_err();
                    self.status = match r {
                        Ok(m) => m,
                        Err(e) => format!("Not moved: {e}"),
                    };
                    let msg = self.status.clone();
                    self.notice(&msg, failed);
                    // The worker has already brought the folders it touched up to date, so the
                    // tree and list are read again from this PC's copy; only a failure needs a
                    // full sync, to put back what was taken off the screen too early.
                    self.keep_status = true;
                    if failed {
                        if !self.syncing {
                            self.sync(self.account);
                        }
                    } else {
                        self.refresh_keeping_place();
                    }
                    // An undo waiting for its delete: Trash may have it now.
                    if let Some((u, since, _)) = self.undo_wait.take() {
                        self.finish_undo(u, since);
                    }
                }
                Event::Downloading(email, here, all) => {
                    self.offline_progress = (here < all).then_some((email, here, all));
                }
                Event::Downloaded(email, r) => {
                    self.offline_progress = None;
                    if let Err(e) = r {
                        self.status = format!("{email}: some mail could not be downloaded to this PC ({e}); it is tried again at the next sync.");
                    }
                }
                Event::SearchProgress(done, all) => {
                    if self.searching {
                        self.search_progress = Some((done, all));
                    }
                }
                Event::Searched(scope, words, r) => {
                    self.searching = false;
                    self.search_progress = None;
                    match r {
                        Ok(hits) => {
                            let n: usize = hits.iter().map(|(_, u)| u.len()).sum();
                            let what = if n == 1 { "1 message".to_string() } else { format!("{n} messages") };
                            self.status = match &scope {
                                Some(f) => format!("The server found {what} in {f}."),
                                None if hits.len() == 1 => {
                                    format!("The server found {what}, in {}.", hits.first().map(|h| h.0.as_str()).unwrap_or(""))
                                }
                                None => format!("The server found {what} across {} folders.", hits.len()),
                            };
                            self.server_hits = Some((scope, words, hits));
                        }
                        Err(e) => {
                            self.status = format!("Server search failed: {e}");
                            let msg = self.status.clone();
                            self.notice(&msg, true);
                        }
                    }
                    self.refresh_messages();
                }
                Event::Synced(email, r) => {
                    self.syncing = false;
                    self.status = match r {
                        Ok(_) if std::mem::take(&mut self.keep_status) => self.status.clone(),
                        Ok((rep, rules)) => {
                            // Not the first sync after starting (that is catching up, not news): by
                            // time, as an idle window draws hardly any frames.
                            if rep.new_messages > 0 && self.started.elapsed() > std::time::Duration::from_secs(20) {
                                self.announce_new_mail(&email);
                            }
                            let mut msg = format!("{email}: {} folders, {} new, {} removed.", rep.folders, rep.new_messages, rep.removed);
                            if let Some(r) = rules {
                                msg.push_str(&format!(" {r}."));
                            }
                            msg
                        }
                        Err(e) => {
                            self.needs_sign_in(&email, &e);
                            let msg = format!("{email}: {}", e.trim_start_matches(worker::SIGN_IN_AGAIN));
                            self.notice(&msg, true);
                            msg
                        }
                    };
                    self.refresh_folders();
                    self.run_sweeps();
                    self.run_auto_replies();
                    self.download_account(&email);
                }
                Event::Sent(r) => match r {
                    Ok(msg) => {
                        self.compose = None;
                        self.notice(&msg, false);
                        self.status = msg;
                        self.sync(self.account);
                    }
                    Err(e) => {
                        if let Some(email) = self.current().map(|a| a.email.clone()) {
                            self.needs_sign_in(&email, &e);
                        }
                        if let Some(c) = self.compose.as_mut() {
                            c.sending = false;
                            c.error = Some(e.clone());
                        }
                        self.status = format!("Not sent: {e}");
                        let msg = self.status.clone();
                        self.notice(&msg, true);
                    }
                },
                Event::DraftSaved(id, r) => {
                    if let Some(c) = self.compose.as_mut().filter(|c| c.draft_id == id) {
                        let fp = c.saving.take();
                        match r {
                            Ok(folder) => {
                                c.on_server = true;
                                if let Some(fp) = fp {
                                    c.saved = fp;
                                }
                                c.note = Some(format!("Draft saved in {folder} at {}", chrono::Local::now().format("%H:%M")));
                            }
                            Err(e) => c.note = Some(format!("Draft not saved: {e}")),
                        }
                    }
                    // Show it in the list when the list is Drafts, the window was closed, or Drafts is new.
                    let known = self.folders.iter().any(|f| self.is_drafts(&f.name));
                    if (self.compose.is_none() || !known || self.folder.as_deref().is_some_and(|f| self.is_drafts(f))) && !self.syncing {
                        self.sync(self.account);
                    }
                }
                Event::DraftDeleted(r) => {
                    if let Err(e) = r {
                        self.status = format!("The draft could not be removed from the server: {e}");
                    }
                    if self.folder.as_deref().is_some_and(|f| self.is_drafts(f)) && !self.syncing {
                        self.sync(self.account);
                    }
                }
                Event::Quota(email, r) => self.quota = Some((email, r)),
                Event::Cal(ev) => self.calendar_event(ev),
                Event::Flagged(r) => {
                    // The screen already shows the change; only a refusal needs anything, and
                    // a sync then puts this PC's copy back to what the server has.
                    if let Err(e) = r {
                        self.status = format!("Could not update the message on the server: {e}");
                        if !self.syncing {
                            self.sync(self.account);
                        }
                    }
                }
                Event::Opened(email, folder, uid, r) => self.on_opened(email, folder, uid, r, false),
            }
        }
    }

    /// A message's text is here, from this PC's copy or from the server: show it if it is
    /// still the one wanted, and log how long it took to get on screen.
    fn on_opened(&mut self, email: String, folder: String, uid: u32, r: Result<Vec<u8>, String>, from_pc: bool) {
        let same_account = self.current().is_some_and(|a| a.email == email);
        if self.loading == Some(uid) {
            self.loading = None;
        }
        match r {
            Ok(raw) if same_account && self.selected.as_ref().is_some_and(|(f, u)| *f == folder && *u == uid) => {
                let drawn = std::time::Instant::now();
                self.show_message(folder.clone(), uid, &raw);
                let since = self.open_asked.take().filter(|(m, _)| m.0 == folder && m.1 == uid).map(|(_, at)| at.elapsed().as_millis());
                log::info!(
                    "show {email} {folder} #{uid}: on screen {} ms after it was asked for ({} KB {}; drawing took {} ms)",
                    since.map_or("?".into(), |ms| ms.to_string()),
                    raw.len() / 1024,
                    if from_pc { "from this PC" } else { "from the server" },
                    drawn.elapsed().as_millis()
                );
                let unread = self.row_unread(&folder, uid);
                // Reading it marks it read: at once, after a few seconds, or never (Settings > Reading pane).
                self.read_timer = None;
                if unread {
                    match self.settings.mark_read {
                        settings::MarkRead::OnOpen => self.mark_read_now(&folder, uid),
                        settings::MarkRead::AfterSeconds => self.read_timer = Some(((folder.clone(), uid), std::time::Instant::now())),
                        settings::MarkRead::Never => {}
                    }
                }
                // A read receipt, the first time it is read (Settings > Message handling).
                self.receipt = None;
                if unread
                    && let Some(o) = &self.open
                    && let Some(to) = o.receipt_to.clone()
                {
                    let r = (to, o.subject.clone(), o.date.clone());
                    match self.settings.receipts {
                        settings::Receipts::Always => self.send_receipt(r),
                        settings::Receipts::Ask => self.receipt = Some(r),
                        settings::Receipts::Never => {}
                    }
                }
            }
            Ok(_) => {
                if same_account && self.head.as_ref().is_some_and(|h| h.iter().any(|(f, u)| *f == folder && *u == uid)) {
                    self.refresh_thread();
                }
            }
            Err(e) => {
                self.status = format!("Could not open the message: {e}");
                let msg = self.status.clone();
                self.notice(&msg, true);
            }
        }
        if !from_pc {
            self.refresh_messages();
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        egui::Panel::top("mc_title").exact_size(38.0).frame(egui::Frame::NONE.fill(t.title_bar).inner_margin(egui::Margin::symmetric(12, 0))).show(
            ui,
            |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("MailCraft").font(semibold(14.0)).color(t.text));
                    ui.label(
                        RichText::new(format!("  {}", self.current().map(|a| a.email.as_str()).unwrap_or(""))).font(regular(12.0)).color(t.text_dim),
                    );
                });
            },
        );
        // The calendar has its own bar; the mail's ribbon is for the mail.
        if self.calendar_active() {
            egui::Panel::bottom("mc_status")
                .exact_size(24.0)
                .frame(egui::Frame::NONE.fill(t.status_bar).inner_margin(egui::Margin::symmetric(10, 4)))
                .show(ui, |ui| {
                    ui.label(RichText::new(&self.status).font(regular(11.5)).color(t.text_dim));
                });
            return;
        }
        let mut tab = self.tab;
        let mut clicked = None;
        let mut fold = self.fold;
        let mut keys = std::mem::take(&mut self.keytips);
        egui::Panel::top("mc_ribbon").exact_size(ribbon::height(&fold)).frame(egui::Frame::NONE.fill(t.ribbon)).show(ui, |ui| {
            let quick: Vec<String> = self.my_quick_steps().into_iter().map(|q| q.name).collect();
            clicked = ribbon::show(ui, &mut tab, &mut fold, t, &quick, &|a| self.state(a), &mut keys);
        });
        self.keytips = keys;
        self.tab = tab;
        self.fold = fold;
        if let Some(a) = clicked {
            let ctx = ui.ctx().clone();
            self.act(a, &ctx);
        }
        egui::Panel::bottom("mc_status")
            .exact_size(24.0)
            .frame(egui::Frame::NONE.fill(t.status_bar).inner_margin(egui::Margin::symmetric(10, 4)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&self.status).font(regular(11.5)).color(t.text_dim));
                    if let Some((_, here, all)) = &self.offline_progress {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let text =
                                format!("Downloading for offline search: {} of {}", settings_ui::thousands(*here), settings_ui::thousands(*all));
                            ui.label(RichText::new(text).font(regular(11.5)).color(t.text_dim));
                        });
                    }
                });
            });
    }

    /// The tabs' keys, as Betterbird's: Ctrl+W (or Ctrl+F4) closes the tab showing (not the
    /// mail's), Ctrl+Tab and Ctrl+Shift+Tab go to the next and previous.
    fn tab_keys(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers as M};
        let (close, prev, next) = ctx.input_mut(|i| {
            let close = i.consume_key(M::COMMAND, Key::W) || i.consume_key(M::COMMAND, Key::F4);
            // The Shift form first: a plain Ctrl+Tab check would take it too.
            let prev = i.consume_key(M::COMMAND | M::SHIFT, Key::Tab);
            let next = !prev && i.consume_key(M::COMMAND, Key::Tab);
            (close, prev, next)
        });
        let n = self.tabs.len().max(1);
        if close && self.active_tab > 0 && self.active_tab < self.tabs.len() {
            self.tabs.remove(self.active_tab);
            self.active_tab = (self.active_tab - 1).min(self.tabs.len().saturating_sub(1));
        }
        if next {
            self.active_tab = (self.active_tab + 1) % n;
        }
        if prev {
            self.active_tab = (self.active_tab + n - 1) % n;
        }
    }

    /// The tabs under the ribbon (Betterbird's): the mail, and Settings and Account settings
    /// when open, each with a cross to close it (the mail's stays).
    /// Take message tab `i` out into a window of its own, at `at` (screen points) if given.
    fn pop_out(&mut self, i: usize, at: Option<egui::Pos2>) {
        let Some(MainTab::Message { folder, uid, .. }) = self.tabs.get(i).cloned() else { return };
        self.tabs.remove(i);
        if self.active_tab >= i {
            self.active_tab = self.active_tab.saturating_sub(1).min(self.tabs.len().saturating_sub(1));
        }
        if self.open.as_ref().is_some_and(|o| o.folder == folder && o.uid == uid) {
            if let Some(msg) = self.open.take() {
                self.add_popout(msg, at);
            }
            // The reading pane shows the selected message again.
            if let Some((f, u)) = self.selected.clone() {
                self.open_message(f, u);
            }
        } else {
            self.pop_pending = Some((folder.clone(), uid, at));
            self.open_message(folder, uid);
        }
    }

    fn add_popout(&mut self, msg: OpenMessage, at: Option<egui::Pos2>) {
        self.pop_seq += 1;
        // Each window's title must be its own (the browser view finds its window by it).
        let base = format!("{} - MailCraft", msg.subject.trim());
        let mut title = base.clone();
        let mut n = 2;
        while self.popouts.iter().any(|p| p.title == title) {
            title = format!("{base} ({n})");
            n += 1;
        }
        self.popouts.push(Popout {
            id: self.pop_seq,
            msg: Some(msg),
            title,
            at: at.map(|p| p - vec2(120.0, 16.0)),
            #[cfg(any(windows, target_os = "linux"))]
            web: None,
            native: None,
            tries: 0,
            web_failed: false,
        });
    }

    /// The popped-out messages' windows (drawn from `logic`, so they stay while the main window
    /// is minimised).
    fn popouts_ui(&mut self, ctx: &egui::Context, t: &Tokens) {
        if self.popouts.is_empty() {
            return;
        }
        if self.card_icon.is_none()
            && let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/app-icon/mailcraft-256.png"))
        {
            let img = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
            self.card_icon = Some(ctx.load_texture("mc_card_icon", img, egui::TextureOptions::LINEAR));
        }
        let css = self.message_css();
        let dark = self.dark_reading();
        let mut acts: Vec<(u64, PopAct)> = vec![];
        let mut note: Option<String> = None;
        let natives: Vec<u64> = self.popouts.iter().filter_map(|p| p.native).collect();
        for p in self.popouts.iter_mut() {
            let mut b =
                egui::ViewportBuilder::default().with_title(p.title.clone()).with_inner_size([880.0, 920.0]).with_min_inner_size([420.0, 300.0]);
            if let Some(at) = p.at {
                b = b.with_position(at);
            }
            if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/app-icon/mailcraft-256.png")) {
                b = b.with_icon(icon);
            }
            let id = p.id;
            ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(("mc_popout", id)), b, |ctx, _class| {
                // Keys pressed in the message itself (Escape, Ctrl+W) count as this window's.
                #[cfg(any(windows, target_os = "linux"))]
                if let Some(w) = p.web.as_ref() {
                    let keys = w.take_keys();
                    if !keys.is_empty() {
                        ctx.input_mut(|i| i.events.extend(keys));
                    }
                }
                if ctx.input_mut(|i| {
                    i.viewport().close_requested() || i.key_pressed(egui::Key::Escape) || i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)
                }) {
                    acts.push((id, PopAct::Close));
                }
                egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ctx, |ui| {
                    let Some(open) = p.msg.as_mut() else { return };
                    egui::Frame::NONE.fill(t.ribbon).inner_margin(egui::Margin::symmetric(18, 12)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            for (label, a) in [("Reply", Action::Reply), ("Reply all", Action::ReplyAll), ("Forward", Action::Forward)] {
                                if ui.button(label).clicked() {
                                    acts.push((id, PopAct::Reply(a)));
                                }
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Back to MailCraft").on_hover_text("Put it back in a tab of the main window").clicked() {
                                    acts.push((id, PopAct::Dock));
                                }
                            });
                        });
                        ui.add_space(8.0);
                        ui.label(RichText::new(&open.subject).font(semibold(18.0)).color(t.text));
                        ui.add_space(6.0);
                        ui.label(RichText::new(&open.from).font(semibold(13.0)).color(t.text));
                        ui.label(RichText::new(format!("To: {}", open.to)).font(regular(12.0)).color(t.text_dim));
                        ui.label(RichText::new(&open.date).font(regular(12.0)).color(t.text_dim));
                        if open.remote && !open.show_remote {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Pictures from the internet are blocked.").font(regular(12.0)).color(t.text_dim));
                                if ui.button("Show pictures").clicked() {
                                    open.show_remote = true;
                                    open.prepared =
                                        htmlview::prepare(&reading_html(&format!("{}{}", open.html, open.thread), dark), &open.inline, true, css);
                                }
                            });
                        }
                        if !open.attachments.is_empty() {
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                for (name, _, bytes) in &open.attachments {
                                    ui.label(
                                        RichText::new(format!("📎 {name} ({} KB)", bytes.len().div_ceil(1024))).font(regular(12.0)).color(t.text),
                                    );
                                    if ui.small_button("Open").clicked() {
                                        note = Some(match open_attachment(name, bytes) {
                                            Ok(()) => format!("Opened {name}."),
                                            Err(e) => e,
                                        });
                                    }
                                    if ui.small_button("Save…").clicked() {
                                        note = Some(match save_attachment(name, bytes) {
                                            Ok(Some(p)) => format!("Saved {p}."),
                                            Ok(None) => String::new(),
                                            Err(e) => format!("Could not save {name}: {e}"),
                                        });
                                    }
                                    ui.add_space(10.0);
                                }
                            });
                        }
                    });
                    #[cfg(any(windows, target_os = "linux"))]
                    {
                        // Its own browser view, in its own window (found by title once it is open).
                        if p.web.is_none() && !p.web_failed {
                            let others: Vec<u64> = natives.iter().copied().filter(|n| Some(*n) != p.native).collect();
                            match mailcraft_window::find_window(&p.title, &others) {
                                Some((native, nid)) => match htmlview::HtmlView::new(&native, ui.ctx().clone()) {
                                    Ok(w) => {
                                        p.web = Some(w);
                                        p.native = Some(nid);
                                    }
                                    Err(_) => p.web_failed = true,
                                },
                                None => {
                                    p.tries += 1;
                                    p.web_failed = p.tries > 120;
                                    ui.ctx().request_repaint();
                                }
                            }
                        }
                        if let Some(w) = p.web.as_mut() {
                            let r = ui.available_rect_before_wrap();
                            ui.allocate_rect(r, Sense::hover());
                            let ppp = ui.ctx().pixels_per_point();
                            w.show(&open.prepared, r.min.x * ppp, r.min.y * ppp, r.width() * ppp, r.height() * ppp);
                            return;
                        }
                    }
                    open.reader.session.view.web_width = ((ui.available_width() - 40.0) / wordcraft_ui_egui::canvas::PX_PER_PT).max(200.0);
                    wordcraft_ui_egui::canvas::show(&mut open.reader, ui);
                });
            });
        }
        if let Some(n) = note.filter(|n| !n.is_empty()) {
            self.status = n;
        }
        for (id, act) in acts {
            let Some(i) = self.popouts.iter().position(|p| p.id == id) else { continue };
            match act {
                PopAct::Close => {
                    // The view goes before its window.
                    let mut p = self.popouts.remove(i);
                    #[cfg(any(windows, target_os = "linux"))]
                    drop(p.web.take());
                    drop(p);
                }
                PopAct::Dock => {
                    let mut p = self.popouts.remove(i);
                    #[cfg(any(windows, target_os = "linux"))]
                    drop(p.web.take());
                    if let Some(msg) = p.msg.take() {
                        let (folder, uid, subject) = (msg.folder.clone(), msg.uid, msg.subject.clone());
                        self.tabs.push(MainTab::Message { folder: folder.clone(), uid, subject });
                        self.active_tab = self.tabs.len() - 1;
                        self.selected = Some((folder, uid));
                        self.open = Some(msg);
                    }
                    ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
                }
                PopAct::Reply(a) => {
                    // Reply from the main window, as if the message were open there.
                    let mine = self.popouts[i].msg.take();
                    let saved = std::mem::replace(&mut self.open, mine);
                    self.act(a, ctx);
                    self.popouts[i].msg = std::mem::replace(&mut self.open, saved);
                    self.active_tab = 0;
                    ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
                }
            }
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let mut pick = None;
        let mut close = None;
        let mut pop: Option<(usize, Option<egui::Pos2>)> = None;
        egui::Panel::top("mc_tabs").exact_size(32.0).frame(egui::Frame::NONE.fill(t.tab_strip)).show(ui, |ui| {
            let bar = ui.max_rect();
            let mut x = bar.left() + 8.0;
            for (i, tab) in self.tabs.iter().enumerate() {
                let (label, kind) = match tab {
                    MainTab::Mail => {
                        let folder = self.folder.as_deref().map(|f| folder_label(f, &self.folders)).unwrap_or_else(|| "Mail".into());
                        let who = self.current().map(|a| a.email.clone()).unwrap_or_default();
                        (if who.is_empty() { folder } else { format!("{folder} - {who}") }, None)
                    }
                    MainTab::Settings { .. } => ("Settings".to_string(), Some(0)),
                    MainTab::Accounts { .. } => ("Account settings".to_string(), Some(1)),
                    MainTab::Calendar => ("Calendar".to_string(), Some(2)),
                    MainTab::Message { subject, .. } => (subject.clone(), Some(3)),
                };
                let on = i == self.active_tab;
                let text_w = ui.painter().layout_no_wrap(label.clone(), semibold(12.5), t.text).size().x.min(260.0);
                let w = 34.0 + text_w + if kind.is_some() { 26.0 } else { 10.0 };
                let r = egui::Rect::from_min_size(pos2(x, bar.top() + 3.0), vec2(w, bar.height() - 3.0));
                let is_msg = matches!(tab, MainTab::Message { .. });
                let resp = ui.interact(r, ui.id().with(("tab", i)), if is_msg { Sense::click_and_drag() } else { Sense::click() });
                if is_msg {
                    // Drag the tab out (off the tab strip, or out of the window) and it opens in a
                    // window of its own where it is let go; or from its menu.
                    if resp.dragged()
                        && let Some(pos) = ui.ctx().pointer_latest_pos()
                    {
                        self.tab_drag = Some((i, pos));
                    }
                    if resp.drag_stopped()
                        && let Some((di, pos)) = self.tab_drag.take()
                        && di == i
                    {
                        // Where it was let go on the desktop (outside the window the app no longer
                        // sees the pointer, so ask the system), in the same points as the window's place.
                        let inner = ui.ctx().input(|inp| inp.viewport().inner_rect);
                        let ppp = ui.ctx().pixels_per_point();
                        let screen = mailcraft_window::cursor_pos().map(|(x, y)| pos2(x as f32 / ppp, y as f32 / ppp));
                        let outside = match (screen, inner) {
                            (Some(s), Some(w)) => !w.contains(s),
                            _ => !ui.ctx().content_rect().shrink(2.0).contains(pos),
                        };
                        let away = outside || pos.y > bar.bottom() + 40.0 || pos.y < bar.top() - 10.0 || ui.ctx().pointer_latest_pos().is_none();
                        if away {
                            pop = Some((i, screen.or(inner.map(|w| w.min + pos.to_vec2()))));
                        }
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Open in a new window").clicked() {
                            pop = Some((i, None));
                            ui.close();
                        }
                    });
                }
                let p = ui.painter();
                if on {
                    p.rect_filled(r, egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, t.ribbon);
                    p.line_segment([r.left_top() + vec2(1.0, 0.0), r.right_top() - vec2(1.0, 0.0)], egui::Stroke::new(2.0, t.accent));
                } else if resp.hovered() {
                    p.rect_filled(r, egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, t.hover);
                }
                let icon = egui::Rect::from_min_size(pos2(r.left() + 10.0, r.center().y - 8.0), vec2(16.0, 16.0));
                match kind {
                    None => foldertree::icon(p, icon, Some(foldertree::Kind::Inbox), t.text, t.accent),
                    Some(2) => calendar::icon(p, icon, if on { t.text } else { t.text_dim }, t.accent),
                    Some(3) => foldertree::icon(p, icon, None, if on { t.text } else { t.text_dim }, t.accent),
                    Some(_) => ribbon::gear(p, icon, if on { t.accent } else { t.text_dim }, t.accent),
                }
                let clip = p.with_clip_rect(egui::Rect::from_min_max(r.min, pos2(r.left() + 32.0 + text_w, r.bottom())));
                clip.text(
                    pos2(r.left() + 32.0, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    &label,
                    if on { semibold(12.5) } else { regular(12.5) },
                    t.text,
                );
                if resp.clicked() {
                    pick = Some(i);
                }
                // Middle-click closes a tab, as in Betterbird.
                if kind.is_some() && resp.middle_clicked() {
                    close = Some(i);
                }
                if kind.is_some() {
                    let xr = egui::Rect::from_center_size(pos2(r.right() - 14.0, r.center().y), vec2(18.0, 18.0));
                    let xresp = ui.interact(xr, ui.id().with(("tabx", i)), Sense::click()).on_hover_text("Close the tab");
                    if xresp.hovered() {
                        ui.painter().rect_filled(xr, 3.0, t.hover);
                    }
                    let c = xr.center();
                    let st = egui::Stroke::new(1.2, t.text_dim);
                    ui.painter().line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], st);
                    ui.painter().line_segment([c + vec2(4.0, -4.0), c + vec2(-4.0, 4.0)], st);
                    if xresp.clicked() {
                        close = Some(i);
                    }
                }
                x = r.right() + 4.0;
            }
            ui.painter().line_segment([bar.left_bottom(), bar.right_bottom()], egui::Stroke::new(1.0, t.border));
            // A message tab on its way out: say what letting go will do.
            if let Some((_, pos)) = self.tab_drag {
                let away = pos.y > bar.bottom() + 40.0 || !ui.ctx().content_rect().shrink(2.0).contains(pos);
                let tip = if away { "Let go to open it in its own window" } else { "Drag out of the tabs to open it in its own window" };
                egui::Area::new(egui::Id::new("mc_tab_drag")).order(egui::Order::Tooltip).fixed_pos(pos + vec2(14.0, 10.0)).show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label(RichText::new(tip).font(regular(12.0)));
                    });
                });
            }
        });
        if !ui.ctx().input(|i| i.pointer.any_down()) && pop.is_none() {
            self.tab_drag = None;
        }
        if let Some((i, at)) = pop {
            self.pop_out(i, at);
            return;
        }
        if let Some(i) = pick {
            self.active_tab = i;
        }
        if let Some(i) = close.filter(|i| *i > 0 && *i < self.tabs.len()) {
            self.tabs.remove(i);
            if self.active_tab >= i {
                self.active_tab = self.active_tab.saturating_sub(1).min(self.tabs.len() - 1);
            }
        }
    }

    /// Betterbird's spaces bar down the left: Mail and Calendar at the top, Settings at the foot.
    fn spaces_bar(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let mut go: Option<u8> = None;
        let on_mail = matches!(self.tabs.get(self.active_tab), Some(MainTab::Mail) | None);
        let on_cal = self.calendar_active();
        let on_settings = matches!(self.tabs.get(self.active_tab), Some(MainTab::Settings { .. } | MainTab::Accounts { .. }));
        egui::Panel::left("mc_spaces").exact_size(44.0).resizable(false).frame(egui::Frame::NONE.fill(t.tab_strip)).show(ui, |ui| {
            let bar = ui.max_rect();
            let button = |ui: &mut egui::Ui, k: u8, at: egui::Pos2, on: bool, tip: &str| {
                let r = egui::Rect::from_center_size(at, vec2(36.0, 36.0));
                let resp = ui.interact(r, ui.id().with(("space", k)), Sense::click());
                let p = ui.painter();
                if on {
                    p.rect_filled(r, 6.0, t.checked);
                    p.rect_filled(egui::Rect::from_min_size(pos2(bar.left(), r.top() + 8.0), vec2(3.0, 20.0)), 1.5, t.accent);
                } else if resp.hovered() {
                    p.rect_filled(r, 6.0, t.hover);
                }
                let icon = egui::Rect::from_center_size(r.center(), vec2(20.0, 20.0));
                let ink = if on { t.text } else { t.text_dim };
                match k {
                    0 => {
                        // A plain envelope.
                        let s = egui::Stroke::new(1.5, ink);
                        let b = egui::Rect::from_center_size(icon.center(), vec2(18.0, 13.0));
                        p.rect_stroke(b, 2.0, s, egui::StrokeKind::Middle);
                        p.add(egui::Shape::line(
                            vec![b.left_top() + vec2(1.0, 1.5), b.center() + vec2(0.0, 1.0), b.right_top() + vec2(-1.0, 1.5)],
                            s,
                        ));
                    }
                    1 => calendar::icon(p, icon, ink, t.accent),
                    _ => ribbon::gear(p, icon, ink, t.accent),
                }
                resp.on_hover_text(tip).clicked()
            };
            if button(ui, 0, pos2(bar.center().x, bar.top() + 26.0), on_mail, "Mail") {
                go = Some(0);
            }
            if button(ui, 1, pos2(bar.center().x, bar.top() + 66.0), on_cal, "Calendar") {
                go = Some(1);
            }
            if button(ui, 2, pos2(bar.center().x, bar.bottom() - 26.0), on_settings, "Settings") {
                go = Some(2);
            }
            ui.painter().line_segment([bar.right_top(), bar.right_bottom()], egui::Stroke::new(1.0, t.border));
        });
        match go {
            Some(0) => self.active_tab = 0,
            Some(1) => self.open_calendar(),
            Some(2) => self.open_settings(settings_ui::Page::MessageList),
            _ => {}
        }
    }

    fn folder_pane(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let shown = egui::Panel::left("mc_folders")
            .default_size(self.folder_w)
            .resizable(true)
            .frame(egui::Frame::NONE.fill(t.panel).inner_margin(egui::Margin::symmetric(0, 6)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut pick_account = None;
                    let mut pick = None;
                    let mut toggle = None;
                    let mut folder_action = None;
                    let mut quiet_toggle: Option<(String, String)> = None;
                    let mut vpick: Option<usize> = None;
                    let mut vedit: Option<usize> = None;
                    let mut vdel: Option<usize> = None;
                    let mut dropped: Option<(Vec<(String, u32)>, String)> = None;
                    let rows = foldertree::rows(&self.folders, &self.collapsed);
                    let width = ui.available_width();
                    // Favourites, as in Outlook: folders picked from any account, at the top.
                    let mut fav_pick: Option<accounts::FolderRef> = None;
                    let mut fav_drop: Option<(Vec<(String, u32)>, String)> = None;
                    let mut unfav: Option<accounts::FolderRef> = None;
                    let mut fav_move: Option<(accounts::FolderRef, accounts::FolderRef, bool)> = None;
                    // A folder dragged in from the tree: (folder, where: next to a favourite (below?) or at the end).
                    let mut fav_add: Option<(accounts::FolderRef, Option<(accounts::FolderRef, bool)>)> = None;
                    // While a folder is dragged the section shows even when empty, as somewhere to drop it.
                    let folder_dragged = egui::DragAndDrop::has_payload_of_type::<DragFolder>(ui.ctx());
                    if self.settings.show_favourites && (!self.fav_rows.is_empty() || folder_dragged) {
                        let open = !self.collapsed.contains("favourites:");
                        let (rect, resp) = ui.allocate_exact_size(vec2(width, 28.0), Sense::click());
                        let over = folder_dragged && resp.dnd_hover_payload::<DragFolder>().is_some();
                        if over {
                            ui.painter().rect_filled(rect, 3.0, t.accent.linear_multiply(0.25));
                            ui.painter().rect_stroke(rect.shrink(1.0), 3.0, egui::Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
                        } else if resp.hovered() {
                            ui.painter().rect_filled(rect, 0.0, t.hover);
                        }
                        if folder_dragged && let Some(d) = resp.dnd_release_payload::<DragFolder>() {
                            fav_add = Some((d.0.clone(), None));
                        }
                        let p = ui.painter();
                        foldertree::twisty(p, rect.left_center() + vec2(12.0, 0.0), open, t.text_dim);
                        let title = if self.fav_rows.is_empty() { "Favourites (drop a folder here)" } else { "Favourites" };
                        p.text(rect.left_center() + vec2(26.0, 0.0), egui::Align2::LEFT_CENTER, title, semibold(12.5), t.text);
                        if resp.clicked() {
                            toggle = Some("favourites:".to_string());
                        }
                        let current = self.current().map(|a| a.email.clone()).unwrap_or_default();
                        let several = self.accounts.len() > 1;
                        for (r, row) in self.fav_rows.iter().filter(|_| open) {
                            let (rect, resp) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click_and_drag());
                            let here = r.account == current && self.folder.as_deref() == Some(r.folder.as_str());
                            // Messages can be dropped on a favourite of the account being shown.
                            let dropping = r.account == current && resp.dnd_hover_payload::<DragMail>().is_some();
                            // Check the kind first: taking the payload as the wrong type throws it
                            // away (egui takes, then checks), and a dragged favourite would be lost.
                            if r.account == current
                                && egui::DragAndDrop::has_payload_of_type::<DragMail>(ui.ctx())
                                && let Some(pl) = resp.dnd_release_payload::<DragMail>()
                            {
                                fav_drop = Some((pl.0.clone(), r.folder.clone()));
                            }
                            if dropping {
                                ui.painter().rect_filled(rect, 3.0, t.accent.linear_multiply(0.25));
                            } else if here {
                                ui.painter().rect_filled(rect, 0.0, t.checked);
                            } else if resp.hovered() {
                                ui.painter().rect_filled(rect, 0.0, t.hover);
                            }
                            let p = ui.painter();
                            let kind = row.as_ref().map(foldertree::kind).unwrap_or(foldertree::Kind::Folder);
                            let tint = self.folder_prefs.colour(&r.account, &r.folder).map(|[r, g, b]| egui::Color32::from_rgb(r, g, b));
                            foldertree::icon_tinted(
                                p,
                                Rect::from_min_size(pos2(rect.left() + 26.0, rect.center().y - 8.0), vec2(16.0, 16.0)),
                                Some(kind),
                                t.text_dim,
                                t.accent,
                                tint,
                            );
                            let name = match row {
                                Some(f) if kind == foldertree::Kind::Inbox => "Inbox".to_string(),
                                Some(f) => f
                                    .delimiter
                                    .as_deref()
                                    .filter(|d| !d.is_empty())
                                    .and_then(|d| f.name.rsplit(d).next())
                                    .unwrap_or(&f.name)
                                    .to_string(),
                                None => r.folder.clone(),
                            };
                            let unread = row.as_ref().map(|f| f.unread).unwrap_or(0);
                            let font = if unread > 0 { semibold(12.5) } else { regular(12.5) };
                            let end = p.text(pos2(rect.left() + 48.0, rect.center().y), egui::Align2::LEFT_CENTER, &name, font.clone(), t.text);
                            let mut x = end.right() + 5.0;
                            let total = row.as_ref().map(|f| f.total).unwrap_or(0);
                            if self.settings.count_total && total > 0 {
                                let c = if unread > 0 { t.blue } else { t.text_dim };
                                x = p.text(pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, format!("({total})"), regular(12.0), c).right() + 5.0;
                            } else if unread > 0 {
                                x = p.text(pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, format!("({unread})"), font, t.blue).right() + 5.0;
                            }
                            if several {
                                p.text(pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, &r.account, regular(11.0), t.text_dim);
                            }
                            // Right-click opens it too (unless it is open already), so the menu's
                            // folder commands act on it, as in the tree.
                            let here = self.accounts.get(self.account).is_some_and(|a| a.email == r.account)
                                && self.folder.as_deref() == Some(r.folder.as_str())
                                && self.vfolder.is_none();
                            if resp.clicked() || (resp.secondary_clicked() && !here) {
                                fav_pick = Some(r.clone());
                            }
                            // Drag a favourite onto another to reorder: it lands above or below
                            // that one, whichever half the pointer is in; a line shows where.
                            if resp.drag_started() {
                                resp.dnd_set_drag_payload(DragFav(r.clone(), name.clone()));
                            }
                            let below = ui.ctx().pointer_latest_pos().is_some_and(|pos| pos.y > rect.center().y);
                            let folder_over = folder_dragged && resp.dnd_hover_payload::<DragFolder>().is_some();
                            if folder_over || resp.dnd_hover_payload::<DragFav>().is_some_and(|d| d.0 != *r) {
                                let y = if below { rect.bottom() } else { rect.top() };
                                ui.painter()
                                    .line_segment([pos2(rect.left() + 20.0, y), pos2(rect.right() - 8.0, y)], egui::Stroke::new(2.0, t.accent));
                            }
                            if folder_dragged && let Some(d) = resp.dnd_release_payload::<DragFolder>() {
                                fav_add = Some((d.0.clone(), Some((r.clone(), below))));
                            }
                            if egui::DragAndDrop::has_payload_of_type::<DragFav>(ui.ctx())
                                && let Some(d) = resp.dnd_release_payload::<DragFav>()
                                && d.0 != *r
                            {
                                fav_move = Some((d.0.clone(), r.clone(), below));
                            }
                            // Everything the tree's menu has, with Remove from Favourites first.
                            resp.context_menu(|ui| {
                                if menu_item(ui, "Remove from Favourites") {
                                    unfav = Some(r.clone());
                                    ui.close();
                                }
                                ui.separator();
                                if let Some(a) = menu(ui, FOLDER_MENU, &|a| self.state(a)) {
                                    folder_action = Some(a);
                                }
                                ui.separator();
                                ui.menu_button("Colour", |ui| {
                                    if let Some(c) = colour_menu(ui) {
                                        folder_action = Some(Action::FolderColour(c));
                                        ui.close();
                                    }
                                });
                            });
                        }
                        ui.add_space(4.0);
                    }
                    for (i, a) in self.accounts.iter().enumerate() {
                        // The account header: open only for the account being shown.
                        let open = i == self.account && !self.collapsed.contains(&format!("account:{}", a.email));
                        let (rect, resp) = ui.allocate_exact_size(vec2(width, 28.0), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(rect, 0.0, t.hover);
                        }
                        let p = ui.painter();
                        foldertree::twisty(p, rect.left_center() + vec2(12.0, 0.0), open, t.text_dim);
                        // The account's name and colour (Account settings), as Betterbird shows them.
                        let tint = a.colour.map(|[r, g, b]| egui::Color32::from_rgb(r, g, b));
                        foldertree::icon_tinted(
                            p,
                            Rect::from_min_size(rect.left_center() + vec2(22.0, -8.0), vec2(16.0, 16.0)),
                            None,
                            t.text,
                            t.accent,
                            tint,
                        );
                        p.text(rect.left_center() + vec2(44.0, 0.0), egui::Align2::LEFT_CENTER, a.title(), semibold(12.5), tint.unwrap_or(t.text));
                        if resp.clicked() {
                            if i == self.account {
                                toggle = Some(format!("account:{}", a.email));
                            } else {
                                pick_account = Some(i);
                            }
                        }
                        if !open {
                            continue;
                        }
                        for r in &rows {
                            let f = r.folder.and_then(|i| self.folders.get(i));
                            let (rect, resp) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click_and_drag());
                            // Drag a folder up into Favourites.
                            if resp.drag_started()
                                && let Some(f) = f
                            {
                                resp.dnd_set_drag_payload(DragFolder(
                                    accounts::FolderRef { account: a.email.clone(), folder: f.name.clone() },
                                    r.label.clone(),
                                ));
                            }
                            let selected = self.vfolder.is_none() && f.is_some_and(|f| self.folder.as_deref() == Some(f.name.as_str()));
                            // Messages dragged over a folder: show where they would land.
                            let dropping = f.is_some() && resp.dnd_hover_payload::<DragMail>().is_some();
                            if let Some(f) = f
                                && egui::DragAndDrop::has_payload_of_type::<DragMail>(ui.ctx())
                                && let Some(p) = resp.dnd_release_payload::<DragMail>()
                            {
                                dropped = Some((p.0.clone(), f.name.clone()));
                            }
                            if dropping {
                                ui.painter().rect_filled(rect, 3.0, t.accent.linear_multiply(0.25));
                                ui.painter().rect_stroke(rect.shrink(1.0), 3.0, egui::Stroke::new(1.5, t.accent), egui::StrokeKind::Inside);
                            } else if selected {
                                ui.painter().rect_filled(rect, 0.0, t.checked);
                            } else if resp.hovered() {
                                ui.painter().rect_filled(rect, 0.0, t.hover);
                            }
                            let x = rect.left() + 14.0 + r.depth as f32 * 16.0;
                            let p = ui.painter();
                            let twisty_hit = r.has_children && resp.clicked() && resp.interact_pointer_pos().is_some_and(|pos| pos.x < x + 12.0);
                            if r.has_children {
                                foldertree::twisty(p, pos2(x + 4.0, rect.center().y), !self.collapsed.contains(&r.full), t.text_dim);
                            }
                            foldertree::icon_tinted(
                                p,
                                Rect::from_min_size(pos2(x + 12.0, rect.center().y - 8.0), vec2(16.0, 16.0)),
                                Some(r.kind),
                                t.text_dim,
                                t.accent,
                                self.folder_prefs.colour(&a.email, &r.full).map(|[r, g, b]| egui::Color32::from_rgb(r, g, b)),
                            );
                            let unread = f.map(|f| f.unread).unwrap_or(0);
                            let font = if unread > 0 { semibold(12.5) } else { regular(12.5) };
                            let colour = if r.folder.is_none() { t.text_dim } else { t.text };
                            let end = p.text(pos2(x + 34.0, rect.center().y), egui::Align2::LEFT_CENTER, &r.label, font.clone(), colour);
                            // Unread count (bold, blue) or, by setting, how many in all (Settings > Folder pane).
                            let total = f.map(|f| f.total).unwrap_or(0);
                            if self.settings.count_total && total > 0 {
                                let c = if unread > 0 { t.blue } else { t.text_dim };
                                p.text(pos2(end.right() + 5.0, rect.center().y), egui::Align2::LEFT_CENTER, format!("({total})"), regular(12.0), c);
                            } else if unread > 0 {
                                p.text(pos2(end.right() + 5.0, rect.center().y), egui::Align2::LEFT_CENTER, format!("({unread})"), font, t.blue);
                            }
                            if twisty_hit || (resp.clicked() && f.is_none()) {
                                toggle = Some(r.full.clone());
                            } else if (resp.clicked() || resp.secondary_clicked())
                                && let Some(f) = f
                            {
                                pick = Some(f.name.clone());
                            }
                            if f.is_some() {
                                resp.context_menu(|ui| {
                                    if let Some(a) = menu(ui, FOLDER_MENU, &|a| self.state(a)) {
                                        folder_action = Some(a);
                                    }
                                    ui.separator();
                                    let fav = f.is_some_and(|f| self.folder_prefs.is_favourite(&a.email, &f.name));
                                    if menu_item(ui, if fav { "Remove from Favourites" } else { "Add to Favourites" }) {
                                        folder_action = Some(Action::ToggleFavourite);
                                        ui.close();
                                    }
                                    // New-mail notifications for this folder (Settings > Notifications).
                                    if let Some(f) = f.filter(|f| {
                                        matches!(foldertree::kind(f), foldertree::Kind::Inbox | foldertree::Kind::Folder)
                                            && self.settings.notifications
                                            && (self.settings.notify_all_folders || foldertree::kind(f) == foldertree::Kind::Inbox)
                                    }) {
                                        let on = !self.folder_prefs.is_quiet(&a.email, &f.name);
                                        let label = if on { "Don't notify about new mail here" } else { "Notify about new mail here" };
                                        if menu_item(ui, label) {
                                            quiet_toggle = Some((a.email.clone(), f.name.clone()));
                                            ui.close();
                                        }
                                    }
                                    ui.menu_button("Colour", |ui| {
                                        if let Some(c) = colour_menu(ui) {
                                            folder_action = Some(Action::FolderColour(c));
                                            ui.close();
                                        }
                                    });
                                });
                            }
                        }
                        // Search folders, as in Outlook: saved searches over the whole account.
                        let sfs: Vec<(usize, String)> = self
                            .settings
                            .search_folders
                            .iter()
                            .enumerate()
                            .filter(|(_, x)| x.account == a.email)
                            .map(|(i, x)| (i, x.name.clone()))
                            .collect();
                        if !sfs.is_empty() {
                            let open_sf = !self.collapsed.contains("searchfolders:");
                            let (rect, resp) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click());
                            if resp.hovered() {
                                ui.painter().rect_filled(rect, 0.0, t.hover);
                            }
                            foldertree::twisty(ui.painter(), pos2(rect.left() + 18.0, rect.center().y), open_sf, t.text_dim);
                            ui.painter().text(
                                pos2(rect.left() + 30.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                "Search folders",
                                regular(12.5),
                                t.text_dim,
                            );
                            if resp.clicked() {
                                toggle = Some("searchfolders:".into());
                            }
                            for (i, name) in sfs.iter().filter(|_| open_sf) {
                                let (rect, resp) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click());
                                if self.vfolder == Some(*i) {
                                    ui.painter().rect_filled(rect, 0.0, t.checked);
                                } else if resp.hovered() {
                                    ui.painter().rect_filled(rect, 0.0, t.hover);
                                }
                                let p = ui.painter();
                                let c = pos2(rect.left() + 38.0, rect.center().y - 1.0);
                                p.circle_stroke(c, 4.5, egui::Stroke::new(1.3, t.accent));
                                p.line_segment([c + vec2(3.2, 3.2), c + vec2(6.5, 6.5)], egui::Stroke::new(1.5, t.accent));
                                p.text(pos2(rect.left() + 52.0, rect.center().y), egui::Align2::LEFT_CENTER, name, regular(12.5), t.text);
                                if resp.clicked() {
                                    vpick = Some(*i);
                                }
                                resp.context_menu(|ui| {
                                    if menu_item(ui, "Edit search folder...") {
                                        vedit = Some(*i);
                                        ui.close();
                                    }
                                    if menu_item(ui, "Delete search folder") {
                                        vdel = Some(*i);
                                        ui.close();
                                    }
                                });
                            }
                        }
                    }
                    if let Some(i) = vpick {
                        self.vfolder = Some(i);
                        self.selected = None;
                        self.picked.clear();
                        self.open = None;
                        self.search.clear();
                        self.server_hits = None;
                        self.refresh_messages();
                    }
                    if let Some(i) = vedit {
                        self.set_sel = Some(i);
                        self.open_settings(settings_ui::Page::SearchFolders);
                    }
                    if let Some(i) = vdel.filter(|i| *i < self.settings.search_folders.len()) {
                        self.settings.search_folders.remove(i);
                        let _ = settings::save(&self.settings);
                        if self.vfolder.is_some() {
                            self.vfolder = None;
                            self.refresh_messages();
                        }
                    }
                    if let Some(k) = toggle
                        && !self.collapsed.remove(&k)
                    {
                        self.collapsed.insert(k);
                    }
                    if let Some(p) = pick.filter(|p| self.vfolder.is_some() || self.folder.as_deref() != Some(p.as_str())) {
                        self.vfolder = None;
                        self.folder = Some(p);
                        self.selected = None;
                        self.open = None;
                        self.search.clear();
                        self.server_hits = None;
                        self.refresh_messages();
                    }
                    if let Some(a) = folder_action {
                        let ctx = ui.ctx().clone();
                        self.act(a, &ctx);
                    }
                    if let Some((acct, folder)) = quiet_toggle {
                        self.folder_prefs.toggle_quiet(&acct, &folder);
                        self.save_folder_prefs();
                        let state = if self.folder_prefs.is_quiet(&acct, &folder) { "off" } else { "on" };
                        self.status = format!("New-mail notifications for {folder}: {state}.");
                    }
                    if let Some((folder, place)) = fav_add {
                        let favs = &mut self.folder_prefs.favourites;
                        favs.retain(|x| *x != folder);
                        let at = place
                            .and_then(|(target, below)| favs.iter().position(|x| *x == target).map(|i| if below { i + 1 } else { i }))
                            .unwrap_or(favs.len());
                        favs.insert(at.min(favs.len()), folder);
                        self.collapsed.remove("favourites:");
                        self.save_folder_prefs();
                    }
                    if let Some((moving, target, below)) = fav_move {
                        let favs = &mut self.folder_prefs.favourites;
                        if let Some(from) = favs.iter().position(|x| *x == moving) {
                            let item = favs.remove(from);
                            let at = favs.iter().position(|x| *x == target).map(|i| if below { i + 1 } else { i }).unwrap_or(favs.len());
                            favs.insert(at.min(favs.len()), item);
                            self.save_folder_prefs();
                        }
                    }
                    if let Some(r) = unfav {
                        self.folder_prefs.favourites.retain(|x| *x != r);
                        self.save_folder_prefs();
                    }
                    if let Some((items, to)) = fav_drop {
                        let copy = self.drag_copy || ui.input(|i| i.modifiers.command);
                        self.move_to(items, to, copy);
                    }
                    if let Some(r) = fav_pick
                        && let Some(idx) = self.accounts.iter().position(|a| a.email == r.account)
                    {
                        if idx != self.account {
                            self.account = idx;
                            self.folders.clear();
                        }
                        self.folder = Some(r.folder);
                        self.selected = None;
                        self.picked.clear();
                        self.open = None;
                        self.search.clear();
                        self.server_hits = None;
                        self.refresh_folders();
                    }
                    if let Some((items, to)) = dropped {
                        // Ctrl held while dropping copies, as in Outlook. Ctrl let go in the same
                        // instant as the mouse still counts: use the frame before the drop as well.
                        let copy = self.drag_copy || ui.input(|i| i.modifiers.command);
                        self.move_to(items, to, copy);
                    }
                    if let Some(i) = pick_account {
                        self.account = i;
                        self.folder = None;
                        self.open = None;
                        self.search.clear();
                        self.server_hits = None;
                        self.refresh_folders();
                    }
                });
            });
        self.folder_w = shown.response.rect.width();
    }

    /// The message list: beside the reading pane, above it, or (reading pane off) on its own.
    fn list_pane(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let frame = egui::Frame::NONE.fill(t.ribbon).inner_margin(egui::Margin::same(0));
        match self.pane_at {
            PaneAt::Right => {
                let shown =
                    egui::Panel::left("mc_list").default_size(self.list_w).resizable(true).frame(frame).show(ui, |ui| self.list_contents(ui, t));
                self.list_w = shown.response.rect.width();
            }
            PaneAt::Bottom => {
                let shown =
                    egui::Panel::top("mc_list_top").default_size(self.list_h).resizable(true).frame(frame).show(ui, |ui| self.list_contents(ui, t));
                self.list_h = shown.response.rect.height();
            }
            PaneAt::Off => {
                egui::CentralPanel::default().frame(frame).show(ui, |ui| self.list_contents(ui, t));
            }
        }
    }

    fn list_contents(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        // Where searches look (Settings > Search), set afresh on each folder.
        if self.scope_for != self.folder {
            self.scope_for = self.folder.clone();
            let all = match self.settings.search_scope {
                settings::SearchScope::All => true,
                settings::SearchScope::Current => false,
                settings::SearchScope::FromInbox => self.folder.as_deref().is_some_and(|f| self.kind_of(f) == foldertree::Kind::Inbox),
            };
            if all != self.search_all {
                self.search_all = all;
                if !self.search.trim().is_empty() {
                    self.refresh_messages();
                }
            }
        }
        {
            {
                egui::Frame::NONE.inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
                    // Clear goes in first from the right, so the box takes exactly what is left
                    // (a box that asks for more than that makes the panel widen itself).
                    ui.allocate_ui_with_layout(vec2(ui.available_width(), 24.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.search.is_empty() && ui.small_button("Clear").clicked() {
                            self.search.clear();
                            self.server_hits = None;
                            self.refresh_messages();
                        }
                        let w = ui.available_width() - 10.0;
                        let had_focus = ui.memory(|m| m.has_focus(egui::Id::new("mc_search")));
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.search)
                                .id(egui::Id::new("mc_search"))
                                .desired_width(w)
                                .hint_text(if self.search_all { "Search all folders" } else { "Search this folder" }),
                        );
                        if std::mem::take(&mut self.focus_search) {
                            r.request_focus();
                        }
                        if r.changed() {
                            self.server_hits = None;
                            self.refresh_messages();
                        }
                        // Search is of the mail on this PC (its words are indexed as it downloads);
                        // the server is asked only with "Search the server" below.
                        if r.lost_focus() && self.search.trim().chars().count() >= 2 && !self.messages.is_empty() {
                            let words = self.search.clone();
                            self.settings.remember_search(&words);
                            let _ = settings::save(&self.settings);
                        }
                        // Recent searches under the box while it is in use.
                        let typed = self.search.trim().to_lowercase();
                        let recent: Vec<String> = self
                            .settings
                            .search_history
                            .iter()
                            .filter(|h| typed.is_empty() || (h.to_lowercase().starts_with(&typed) && h.to_lowercase() != typed))
                            .take(8)
                            .cloned()
                            .collect();
                        if had_focus && !recent.is_empty() {
                            let mut chosen = None;
                            egui::Area::new(egui::Id::new("mc_search_recent"))
                                .order(egui::Order::Foreground)
                                .fixed_pos(r.rect.left_bottom() + vec2(0.0, 2.0))
                                .show(ui.ctx(), |ui| {
                                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                                        ui.set_width(r.rect.width());
                                        ui.label(RichText::new("Recent searches").font(regular(11.0)).color(t.text_dim));
                                        for h in &recent {
                                            let item =
                                                egui::Button::new(h.as_str()).frame_when_inactive(false).min_size(vec2(r.rect.width() - 12.0, 22.0));
                                            if ui.add(item).clicked() {
                                                chosen = Some(h.clone());
                                            }
                                        }
                                    });
                                });
                            if let Some(h) = chosen {
                                self.search = h.clone();
                                self.settings.remember_search(&h);
                                let _ = settings::save(&self.settings);
                                self.server_hits = None;
                                self.refresh_messages();
                                ui.memory_mut(|m| m.surrender_focus(egui::Id::new("mc_search")));
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut self.search_all, RichText::new("All folders").font(regular(11.5))).changed() {
                            self.server_hits = None;
                            self.refresh_messages();
                        }
                        if self.searching {
                            let how_far = match self.search_progress {
                                Some((done, all)) if all > 1 => format!("Searching the server: folder {} of {all}...", done + 1),
                                _ => "Searching the server...".into(),
                            };
                            ui.label(RichText::new(how_far).font(regular(11.5)).color(t.text_dim));
                            if ui.small_button("Stop").clicked() {
                                self.stop_search();
                            }
                        } else if !self.search.trim().is_empty() {
                            ui.label(RichText::new(format!("{} found", self.messages.len())).font(regular(11.5)).color(t.text_dim));
                            // Mail not downloaded yet can only be found by its headers here.
                            let missing = self.missing_here();
                            if self.server_hits.is_none() {
                                let label = if missing > 0 {
                                    format!("Search the server ({} not on this PC yet)", settings_ui::thousands(missing))
                                } else {
                                    "Search the server".into()
                                };
                                if ui.small_button(label).clicked() {
                                    self.server_search();
                                }
                            }
                        }
                    });
                });
                // Focused Inbox: Focused and Other, as tabs over the Inbox.
                if self.focused_here() {
                    let mut switch = None;
                    let other_unread = if self.show_other { 0 } else { self.other_unread };
                    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 8, top: 0, bottom: 2 }).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (other, label) in [
                                (false, "Focused".to_string()),
                                (true, if other_unread > 0 { format!("Other ({other_unread})") } else { "Other".into() }),
                            ] {
                                let on = self.show_other == other;
                                let r = ui.add(
                                    egui::Label::new(RichText::new(label).font(if on { semibold(13.0) } else { regular(13.0) }).color(if on {
                                        t.text
                                    } else {
                                        t.text_dim
                                    }))
                                    .sense(Sense::click()),
                                );
                                if on {
                                    ui.painter().line_segment(
                                        [r.rect.left_bottom() + vec2(0.0, 3.0), r.rect.right_bottom() + vec2(0.0, 3.0)],
                                        egui::Stroke::new(2.0, t.accent),
                                    );
                                }
                                if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                    switch = Some(other);
                                }
                                ui.add_space(12.0);
                            }
                        });
                    });
                    if let Some(o) = switch
                        && o != self.show_other
                    {
                        self.show_other = o;
                        self.refresh_messages();
                    }
                }
                // Rows (Settings > Message list): several lines per message, one line, or one line
                // when the list is wide; spacing; preview text; attachments under the message.
                let wide = self.pane_at != PaneAt::Right || ui.available_width() > 760.0;
                let compact = match self.settings.rows {
                    settings::Rows::One => true,
                    settings::Rows::BySize => wide,
                    settings::Rows::Multiple => false,
                };
                let (base, l1, l2, gap) = match (compact, self.settings.density) {
                    (true, _) => (28.0, 6.0, 6.0, 0.0),
                    (false, settings::Density::Cozy) => (42.0, 4.0, 22.0, 17.0),
                    (false, _) => (52.0, 8.0, 28.0, 18.0),
                };
                let preview_lines: usize = match (compact, self.settings.preview) {
                    (false, settings::Preview::OneLine) => 1,
                    (false, settings::Preview::TwoLines) => 2,
                    _ => 0,
                };
                let chips = self.settings.attachment_previews && !compact;
                let heights: Vec<f32> = self
                    .lines
                    .iter()
                    .map(|l| match l {
                        Line::Day(_) => 28.0,
                        _ => base + preview_lines as f32 * gap + if chips && !self.line_files(l).is_empty() { 28.0 } else { 0.0 },
                    })
                    .collect();
                let total: f32 = heights.iter().sum();
                let spans_folders = (self.search_all && !self.search.trim().is_empty()) || self.vfolder.is_some();
                let here_folder = self.folder.clone().unwrap_or_default();
                let mut menu_action = None;
                // (line, how it was clicked: 0 plain, 1 Ctrl, 2 Shift, 3 the arrow)
                let mut hit: Option<(usize, u8)> = None;
                // A date heading clicked (its messages, and whether to add them to what is picked).
                let mut day_pick: Option<(Vec<(String, u32)>, bool)> = None;
                let mut double: Option<usize> = None;
                let mut hover_hit: Option<(Vec<(String, u32)>, settings::HoverAction)> = None;
                // One row: column headings, as Outlook's list has (click to sort, again to
                // reverse; drag the edge of From to resize it).
                let extra = self.settings.list_columns.clone();
                // A click in an icon column: (the line's messages, which column, line, a flag for it).
                let mut col_hit: Option<(Vec<(String, u32)>, settings::ListCol, usize, bool)> = None;
                // Every column's width: dragged ones as left, the rest their own; Date and Received
                // at least wide enough for a whole date in the chosen format (25 Oct: the dates
                // at the end were cut off and could not be widened).
                let date_w = {
                    let sample = short_date(&Some("2026-12-28T12:59:00+00:00".to_string()));
                    (ui.painter().layout_no_wrap(sample, regular(11.5), t.text).size().x + 20.0).max(104.0)
                };
                let widths: std::collections::HashMap<settings::ListCol, f32> = settings::ListCol::ALL
                    .iter()
                    .map(|&c| {
                        let own = match c {
                            settings::ListCol::Date | settings::ListCol::Received => date_w,
                            settings::ListCol::From => self.from_col,
                            _ => c.width(self.from_col),
                        };
                        (c, self.col_w.get(&c).copied().unwrap_or(own))
                    })
                    .collect();
                if compact {
                    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
                    ui.painter().rect_filled(bar, 0.0, t.panel);
                    ui.painter().line_segment([bar.left_bottom(), bar.right_bottom()], egui::Stroke::new(1.0, t.border));
                    let cells = list_cells(bar.left(), bar.right(), &extra, &widths);
                    let mut sort_to = None;
                    for (idx, cell) in cells.iter().enumerate() {
                        let r = egui::Rect::from_min_max(pos2(cell.x0, bar.top()), pos2(cell.x1, bar.bottom()));
                        // Drag a heading to move its column (a click still sorts, or selects all).
                        let dresp = ui.interact(r, ui.id().with(("coldrag", idx)), Sense::drag());
                        if dresp.drag_started() {
                            self.col_drag = Some(cell.col.unwrap_or(settings::ListCol::Subject));
                        }
                        if cell.col.is_some_and(|c| c.is_icon()) {
                            let c = r.center();
                            match cell.col {
                                Some(settings::ListCol::Select) => {
                                    let all_on = self.picked.len() > 1 && self.picked.len() >= self.messages.len();
                                    let resp = ui.interact(r, ui.id().with("col_selall"), Sense::click()).on_hover_text("Select all / none");
                                    tick_box(ui.painter(), c, all_on, t);
                                    if resp.clicked() {
                                        col_hit = Some((vec![], settings::ListCol::Select, usize::MAX, all_on));
                                    }
                                }
                                Some(settings::ListCol::Thread) => draw_thread(ui.painter(), c, t.text_dim),
                                Some(settings::ListCol::Flag) => draw_flag(ui.painter(), c + vec2(-5.0, -7.0)),
                                Some(settings::ListCol::Attachment) => draw_clip(ui.painter(), c + vec2(-5.0, -7.0), t.text_dim),
                                Some(settings::ListCol::Read) => {
                                    ui.painter().circle_filled(c, 3.5, t.text_dim);
                                }
                                Some(settings::ListCol::Delete) => foldertree::icon(
                                    ui.painter(),
                                    egui::Rect::from_center_size(c, vec2(16.0, 16.0)),
                                    Some(foldertree::Kind::Trash),
                                    t.text_dim,
                                    t.accent,
                                ),
                                Some(settings::ListCol::Junk) => foldertree::icon_tinted(
                                    ui.painter(),
                                    egui::Rect::from_center_size(c, vec2(16.0, 16.0)),
                                    Some(foldertree::Kind::Junk),
                                    t.text_dim,
                                    t.accent,
                                    Some(t.text_dim),
                                ),
                                _ => {}
                            }
                            continue;
                        }
                        // A text heading; From, Subject, Date (sent) and Size sort.
                        let label = cell.col.map(|c| c.heading()).unwrap_or("Subject");
                        let key = match cell.col {
                            None => Some(SortKey::Subject),
                            Some(settings::ListCol::From | settings::ListCol::Correspondents) => Some(SortKey::From),
                            Some(settings::ListCol::Date | settings::ListCol::Received) => Some(SortKey::Date),
                            Some(settings::ListCol::Size) => Some(SortKey::Size),
                            _ => None,
                        };
                        let resp = ui.interact(r, ui.id().with(("col", label)), Sense::click());
                        if key.is_some() && resp.hovered() {
                            ui.painter().rect_filled(r, 0.0, t.hover);
                        }
                        let right = right_aligned(cell.col);
                        let (pos, align) = if right {
                            (pos2(r.right() - 8.0, r.center().y), egui::Align2::RIGHT_CENTER)
                        } else {
                            (pos2(r.left() + 6.0, r.center().y), egui::Align2::LEFT_CENTER)
                        };
                        let on = key.is_some_and(|k| k == self.sort);
                        let clip = ui.painter().with_clip_rect(r.shrink2(vec2(2.0, 0.0)));
                        let g = clip.text(pos, align, label, if on { semibold(12.0) } else { regular(12.0) }, if on { t.text } else { t.text_dim });
                        if on {
                            // Which way: newest/A first points down.
                            let c = pos2(if right { g.left() - 9.0 } else { g.right() + 9.0 }, r.center().y);
                            let tri = if !self.reverse {
                                vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)]
                            } else {
                                vec![c + vec2(-4.0, 2.0), c + vec2(4.0, 2.0), c + vec2(0.0, -3.0)]
                            };
                            ui.painter().add(egui::Shape::convex_polygon(tri, t.text_dim, egui::Stroke::NONE));
                        }
                        // A faint line between text columns, as Betterbird has.
                        ui.painter()
                            .line_segment([pos2(r.right(), r.top() + 6.0), pos2(r.right(), r.bottom() - 6.0)], egui::Stroke::new(1.0, t.border));
                        if let Some(k) = key
                            && resp.on_hover_text(format!("Sort by {}", label.to_lowercase())).clicked()
                        {
                            sort_to = Some(k);
                        }
                    }
                    // A column being moved: a line where it would go, and its name by the pointer.
                    if let Some(moving) = self.col_drag {
                        let x = ui.ctx().pointer_latest_pos().map(|p| p.x).unwrap_or(bar.left());
                        let to = cells.iter().filter(|c| (c.x0 + c.x1) / 2.0 < x).count();
                        let at = cells.get(to).map(|c| c.x0).or(cells.last().map(|c| c.x1)).unwrap_or(bar.left());
                        ui.painter().line_segment([pos2(at, bar.top() + 2.0), pos2(at, bar.bottom() - 2.0)], egui::Stroke::new(2.5, t.accent));
                        let label =
                            egui::Area::new(egui::Id::new("col_drag_label")).order(egui::Order::Tooltip).fixed_pos(pos2(x + 12.0, bar.top() + 4.0));
                        label.show(ui.ctx(), |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.label(RichText::new(moving.name()).font(semibold(12.0)));
                            });
                        });
                        if ui.input(|i| i.pointer.any_released()) {
                            settings::ListCol::move_to(&mut self.settings.list_columns, moving, to);
                            self.col_drag = None;
                            let _ = settings::save(&self.settings);
                        }
                    }
                    // Every edge between two columns: drag to resize, double-click for the column's own
                    // width again. The column left of the edge changes, or, when that is Subject (which
                    // takes the room the others leave), the one to the right of it.
                    let subject_w = cells.iter().find(|c| c.col.is_none()).map(|c| c.x1 - c.x0).unwrap_or(0.0);
                    for (idx, pair) in cells.windows(2).enumerate() {
                        let x = pair[0].x1;
                        let (col, sign) = match (pair[0].col, pair[1].col) {
                            (Some(c), _) => (c, 1.0),
                            (None, Some(c)) => (c, -1.0),
                            (None, None) => continue,
                        };
                        let edge = egui::Rect::from_center_size(pos2(x, bar.center().y), vec2(8.0, bar.height()));
                        let er = ui
                            .interact(edge, ui.id().with(("col_edge", idx)), Sense::click_and_drag())
                            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                            .on_hover_text(format!("Drag to resize {}; double-click for its usual width", col.name().to_lowercase()));
                        if er.hovered() || er.dragged() {
                            ui.painter().line_segment([pos2(x, bar.top() + 2.0), pos2(x, bar.bottom() - 2.0)], egui::Stroke::new(2.0, t.accent));
                        }
                        if er.dragged() {
                            let cur = widths.get(&col).copied().unwrap_or(64.0);
                            // Growing it takes room from Subject, which keeps at least 120.
                            let grow = (sign * er.drag_delta().x).min((subject_w - 120.0).max(0.0));
                            let w = (cur + grow).clamp(col.min_width(), 1200.0);
                            self.col_w.insert(col, w);
                            if col == settings::ListCol::From {
                                self.from_col = w.clamp(100.0, 500.0);
                            }
                        }
                        if er.double_clicked() {
                            self.col_w.remove(&col);
                            if col == settings::ListCol::From {
                                self.from_col = 220.0;
                            }
                        }
                    }
                    // Which columns show: Betterbird's picker, at the right end.
                    let pick = egui::Rect::from_center_size(pos2(bar.right() - 14.0, bar.center().y), vec2(22.0, 22.0));
                    let presp = ui.interact(pick, ui.id().with("col_picker"), Sense::click()).on_hover_text("Choose columns");
                    if presp.hovered() {
                        ui.painter().rect_filled(pick, 3.0, t.hover);
                    }
                    for dx in [-4.0, 0.0, 4.0] {
                        let x = pick.center().x + dx;
                        ui.painter().line_segment([pos2(x, pick.top() + 6.0), pos2(x, pick.bottom() - 6.0)], egui::Stroke::new(1.3, t.text_dim));
                    }
                    let mut changed = false;
                    egui::Popup::menu(&presp).show(|ui| {
                        ui.set_min_width(220.0);
                        for c in settings::ListCol::ALL {
                            if c == settings::ListCol::Subject {
                                // Always there.
                                ui.add_enabled(false, egui::Checkbox::new(&mut true, "Subject"));
                                continue;
                            }
                            let mut on = self.settings.list_columns.contains(&c);
                            if ui.checkbox(&mut on, c.name()).changed() {
                                if on {
                                    settings::ListCol::insert_in_order(&mut self.settings.list_columns, c);
                                } else {
                                    self.settings.list_columns.retain(|x| *x != c);
                                }
                                changed = true;
                            }
                        }
                        ui.separator();
                        ui.label(RichText::new("Drag a heading to move its column.").font(regular(11.0)).color(t.text_dim));
                        ui.separator();
                        if ui.checkbox(&mut self.settings.list_addresses, "Show email addresses").changed() {
                            changed = true;
                        }
                    });
                    if changed {
                        let _ = settings::save(&self.settings);
                    }
                    if let Some(k) = sort_to {
                        if self.sort == k {
                            self.reverse = !self.reverse;
                        } else {
                            self.sort = k;
                            self.reverse = false;
                        }
                        self.refresh_messages();
                    }
                }
                let mut list_area = egui::ScrollArea::vertical().auto_shrink([false, false]);
                if let Some(li) = self.list_reveal.take()
                    && li < heights.len()
                {
                    // Scroll just enough to bring the line the keyboard moved to into view.
                    let top: f32 = heights[..li].iter().sum();
                    let h = heights[li];
                    let (off, view_h, _) = self.list_view;
                    let to = if top < off {
                        Some(top)
                    } else if view_h > 0.0 && top + h > off + view_h {
                        Some(top + h - view_h)
                    } else {
                        None
                    };
                    if let Some(to) = to {
                        list_area = list_area.vertical_scroll_offset(to.max(0.0));
                    }
                }
                list_area.show_viewport(ui, |ui, view| {
                    self.list_view = (view.top(), view.height(), base);
                    let w = ui.available_width();
                    ui.set_min_size(vec2(w, total));
                    let origin = ui.max_rect().min;
                    let mut y = 0.0;
                    for li in 0..self.lines.len() {
                        let h = heights.get(li).copied().unwrap_or(base);
                        let top = y;
                        y += h;
                        if top + h < view.top() {
                            continue;
                        }
                        if top > view.bottom() {
                            break;
                        }
                        let Some(line) = self.lines.get(li) else { continue };
                        let rect = egui::Rect::from_min_size(origin + vec2(0.0, top), vec2(w, h));
                        if let Line::Day(label) = line {
                            // Click a heading to pick everything under it (Ctrl+click adds it to what is
                            // picked), then Delete, Move, etc. act on the lot; or drag it onto a folder
                            // (25 Oct: "so I can delete or move everything that day/week").
                            let group: Vec<(String, u32)> = self.lines[li + 1..]
                                .iter()
                                .take_while(|l| !matches!(l, Line::Day(_)))
                                .flat_map(|l| self.line_items_here(l))
                                .collect();
                            let resp = ui.interact(rect, ui.id().with(("mc_day", li)), Sense::click_and_drag());
                            // Lit when its messages were picked together, not just because a lone message under
                            // it is the one open.
                            let all_on = self.picked.len() > 1 && !group.is_empty() && group.iter().all(|(f, u)| self.is_picked(f, *u));
                            let p = ui.painter();
                            p.rect_filled(
                                rect,
                                0.0,
                                if all_on {
                                    t.checked
                                } else if resp.hovered() {
                                    t.hover
                                } else {
                                    t.ribbon
                                },
                            );
                            p.text(pos2(rect.left() + 12.0, rect.bottom() - 7.0), egui::Align2::LEFT_BOTTOM, label, semibold(12.0), t.text_dim);
                            if resp.hovered() || all_on {
                                let n = format!("{} message{}", group.len(), if group.len() == 1 { "" } else { "s" });
                                p.text(pos2(rect.right() - 12.0, rect.bottom() - 7.0), egui::Align2::RIGHT_BOTTOM, n, regular(11.5), t.text_dim);
                            }
                            p.line_segment([rect.left_bottom(), rect.right_bottom()], egui::Stroke::new(1.0, t.border));
                            if !group.is_empty() {
                                let resp =
                                    resp.on_hover_text(format!("Select everything under {label} (Ctrl+click adds it); or drag it onto a folder"));
                                if resp.drag_started() {
                                    resp.dnd_set_drag_payload(DragMail(group.clone()));
                                    day_pick = Some((group, false));
                                } else if resp.clicked() {
                                    day_pick = Some((group, ui.input(|i| i.modifiers.command)));
                                }
                            }
                            continue;
                        }
                        // The message the line shows; a conversation shows its newest.
                        let (mi, indent, members): (usize, f32, &[usize]) = match line {
                            Line::Msg { i, child } => (*i, if *child { 26.0 } else { 0.0 }, std::slice::from_ref(i)),
                            Line::Head { members, .. } => (members.first().copied().unwrap_or(0), 0.0, members.as_slice()),
                            Line::Day(_) => continue,
                        };
                        let Some(m) = self.row(mi) else { continue };
                        let is_head = matches!(line, Line::Head { .. });
                        let open_head = matches!(line, Line::Head { open: true, .. });
                        let all: Vec<&MessageRow> = members.iter().filter_map(|&i| self.row(i)).collect();
                        let resp = ui.interact(rect, ui.id().with(("mc_line", li)), Sense::click_and_drag());
                        let sel = self.line_selected(line);
                        let bg = if sel {
                            t.checked
                        } else if resp.hovered() {
                            t.hover
                        } else {
                            t.ribbon
                        };
                        ui.painter().rect_filled(rect, 0.0, bg);
                        let unread = all.iter().any(|x| !x.seen());
                        if unread {
                            ui.painter().rect_filled(egui::Rect::from_min_size(rect.min + vec2(indent, 0.0), vec2(3.0, h)), 0.0, t.accent);
                        }
                        let p = ui.painter();
                        if !compact {
                            // One row: the columns, as the headings have them.
                            let lead_w = 0.0;
                            let mut x0 = rect.left() + 12.0 + lead_w + indent + if is_head { 14.0 } else { 0.0 };
                            if is_head {
                                foldertree::twisty(p, pos2(rect.left() + 12.0 + lead_w, rect.top() + l1 + 8.0), open_head, t.text_dim);
                            }
                            // A conversation names who is in it (newest first) and how many messages.
                            let from = if is_head {
                                let mut names: Vec<String> = vec![];
                                for x in &all {
                                    let n = x.from_name.clone().or(x.from_addr.clone()).unwrap_or_default();
                                    if !n.is_empty() && !names.contains(&n) {
                                        names.push(n);
                                    }
                                }
                                let more = names.len().saturating_sub(3);
                                let mut s = names.into_iter().take(3).collect::<Vec<_>>().join("; ");
                                if more > 0 {
                                    s.push_str(&format!(" +{more}"));
                                }
                                s
                            } else if compact && self.settings.list_addresses && m.from_name.is_some() && m.from_addr.is_some() {
                                format!("{} <{}>", m.from_name.as_deref().unwrap_or(""), m.from_addr.as_deref().unwrap_or(""))
                            } else {
                                m.from_name.clone().or(m.from_addr.clone()).unwrap_or_else(|| "(unknown sender)".into())
                            };
                            // Sender pictures: initials in a colour of their own (nothing is fetched).
                            if self.settings.sender_images {
                                let d = if compact { 20.0 } else { 32.0 };
                                let who = m.from_name.clone().or(m.from_addr.clone()).unwrap_or_default();
                                let c = pos2(x0 + d / 2.0, rect.top() + if compact { base / 2.0 } else { (l1 + l2 + 16.0) / 2.0 });
                                p.circle_filled(c, d / 2.0, sender_colour(m.from_addr.as_deref().unwrap_or(&who)));
                                p.text(
                                    c,
                                    egui::Align2::CENTER_CENTER,
                                    initials(&who),
                                    semibold(if compact { 9.0 } else { 12.5 }),
                                    egui::Color32::WHITE,
                                );
                                x0 += d + 10.0;
                            }
                            // Conditional formatting (Settings): the first format the message matches.
                            let fmt = {
                                let who = format!("{} {}", m.from_name.as_deref().unwrap_or(""), m.from_addr.as_deref().unwrap_or(""));
                                self.settings.formats.iter().find(|f| f.matches(&who, m.subject.as_deref().unwrap_or(""), unread))
                            };
                            let fmt_col = fmt.map(|f| egui::Color32::from_rgb(f.colour[0], f.colour[1], f.colour[2]));
                            let bold = unread || fmt.is_some_and(|f| f.bold);
                            let subj = m.subject.clone().unwrap_or_else(|| "(no subject)".into());
                            let preview = m.preview.clone().filter(|p| !p.is_empty() && self.settings.preview != settings::Preview::Off);
                            let files = if chips || compact { self.line_files(line) } else { vec![] };
                            let date_w = p
                                .text(rect.right_top() + vec2(-10.0, l1), egui::Align2::RIGHT_TOP, short_date(&m.date), regular(11.0), t.text_dim)
                                .width();
                            let has_flag = all.iter().any(|x| flagged(x));
                            let show_folder = spans_folders || (!is_head && m.folder != here_folder);
                            // Where line 2's text must stop: before the flag, the folder, (one line) the date.
                            // Flag and attachment show inline unless they have columns of their own.
                            let flag_inline = has_flag && !(compact && extra.contains(&settings::ListCol::Flag));
                            let clip_inline = compact && !files.is_empty() && !extra.contains(&settings::ListCol::Attachment);
                            let right_base = rect.right() - 10.0;
                            let mut limit2 = right_base - if flag_inline { 18.0 } else { 0.0 };
                            if show_folder {
                                limit2 -= p.layout_no_wrap(folder_label(&m.folder, &self.folders), regular(11.0), t.text_dim).size().x + 8.0;
                            }
                            if clip_inline {
                                limit2 -= 16.0;
                                draw_clip(p, pos2(limit2 + 4.0, rect.top() + 7.0), t.text_dim);
                            }
                            // One row: fixed columns under the headings (From | Subject | Received).
                            let col_x = x0;
                            let limit1 = rect.right() - 18.0 - date_w;
                            let clip1 = p.with_clip_rect(egui::Rect::from_min_max(pos2(x0, rect.top()), pos2(limit1, rect.bottom())));
                            let clip2 = p.with_clip_rect(egui::Rect::from_min_max(pos2(x0, rect.top()), pos2(limit2, rect.bottom())));
                            // Who it is from and what it is about: sender first, or (Settings) subject first.
                            let from_col = fmt_col.unwrap_or(t.text);
                            let subj_col = fmt_col.unwrap_or(if unread { t.blue } else { t.text_dim });
                            let (first, first_col, second, second_col) = if self.settings.subject_first {
                                (subj.clone(), fmt_col.unwrap_or(if unread { t.blue } else { t.text }), from.clone(), fmt_col.unwrap_or(t.text_dim))
                            } else {
                                (from.clone(), from_col, subj.clone(), subj_col)
                            };
                            let end = clip1.text(
                                pos2(x0, rect.top() + l1),
                                egui::Align2::LEFT_TOP,
                                first,
                                if bold { semibold(13.0) } else { regular(13.0) },
                                first_col,
                            );
                            if is_head {
                                clip1.text(
                                    pos2(end.right() + 6.0, rect.top() + l1 + 1.0),
                                    egui::Align2::LEFT_TOP,
                                    format!("({})", all.len()),
                                    regular(12.0),
                                    t.text_dim,
                                );
                            }
                            // One line: the second item follows the first; then the preview, dimmed.
                            let x2 = if compact { col_x } else { x0 };
                            let end2 = clip2.text(
                                pos2(x2, rect.top() + l2),
                                egui::Align2::LEFT_TOP,
                                second,
                                if bold && !self.settings.subject_first { semibold(12.0) } else { regular(12.0) },
                                second_col,
                            );
                            let inline = compact || self.settings.preview == settings::Preview::Inline;
                            if inline && let Some(pv) = &preview {
                                clip2.text(pos2(end2.right() + 8.0, rect.top() + l2), egui::Align2::LEFT_TOP, pv, regular(12.0), t.text_dim);
                            }
                            // Preview on lines of its own under the subject.
                            if preview_lines > 0
                                && let Some(pv) = &preview
                            {
                                let mut job =
                                    egui::text::LayoutJob::simple(pv.clone(), regular(12.0), t.text_dim, (rect.right() - 10.0 - x0).max(40.0));
                                job.wrap.max_rows = preview_lines;
                                job.wrap.break_anywhere = false;
                                job.wrap.overflow_character = Some('\u{2026}');
                                let g = p.layout_job(job);
                                p.galley(pos2(x0, rect.top() + l2 + gap), g, t.text_dim);
                            }
                            // Attachments as chips under it (Settings > Message list).
                            if chips && !files.is_empty() {
                                let mut x = x0;
                                let cy = rect.top() + base + preview_lines as f32 * gap - 2.0;
                                for (k, name) in files.iter().enumerate() {
                                    if k == 3 || x > rect.right() - 90.0 {
                                        p.text(pos2(x, cy + 3.0), egui::Align2::LEFT_TOP, format!("+{}", files.len() - k), regular(11.0), t.text_dim);
                                        break;
                                    }
                                    let shown: String = if name.chars().count() > 24 {
                                        format!("{}\u{2026}", name.chars().take(22).collect::<String>())
                                    } else {
                                        name.clone()
                                    };
                                    let tw = p.layout_no_wrap(shown.clone(), regular(11.0), t.text).size().x;
                                    let chip = egui::Rect::from_min_size(pos2(x, cy), vec2(tw + 28.0, 22.0));
                                    p.rect_filled(chip, 4.0, t.panel);
                                    p.rect_stroke(chip, 4.0, egui::Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
                                    draw_clip(p, chip.min + vec2(6.0, 4.0), t.text_dim);
                                    p.text(pos2(chip.left() + 20.0, chip.center().y), egui::Align2::LEFT_CENTER, shown, regular(11.0), t.text);
                                    x = chip.right() + 6.0;
                                }
                            }
                            p.line_segment([rect.left_bottom(), rect.right_bottom()], egui::Stroke::new(1.0, t.border));
                            // Right end of line 2, right to left: flag, folder (searching), categories.
                            let mut right = right_base;
                            if flag_inline {
                                right -= 18.0;
                            }
                            // The folder shows when searching everywhere, and for Sent messages in a conversation.
                            if show_folder {
                                right -= p.layout_no_wrap(folder_label(&m.folder, &self.folders), regular(11.0), t.text_dim).size().x + 8.0;
                            }
                            if clip_inline {
                                right -= 16.0;
                            }
                            let mut cats: Vec<&accounts::Category> = vec![];
                            for x in &all {
                                for c in self.row_categories(x) {
                                    if !cats.iter().any(|k| k.key == c.key) {
                                        cats.push(c);
                                    }
                                }
                            }
                            if !cats.is_empty() {
                                let shown: Vec<&accounts::Category> = cats.iter().take(3).copied().collect();
                                let widths: Vec<f32> =
                                    shown.iter().map(|c| p.layout_no_wrap(c.name.clone(), regular(10.5), t.text).size().x + 12.0).collect();
                                let more = cats.len().saturating_sub(shown.len());
                                let total: f32 = widths.iter().sum::<f32>() + 4.0 * widths.len() as f32 + if more > 0 { 24.0 } else { 0.0 };
                                let mut x = (right - total).max(rect.left() + 120.0);
                                // Cover the end of the subject so the tags read cleanly.
                                p.rect_filled(
                                    egui::Rect::from_min_max(pos2(x - 6.0, rect.top() + l2 - 2.0), pos2(right + 2.0, rect.top() + l2 + 16.0)),
                                    0.0,
                                    bg,
                                );
                                for (c, w) in shown.iter().zip(&widths) {
                                    let [r, g, b] = c.colour;
                                    let fill = egui::Color32::from_rgb(r, g, b);
                                    let light = (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000 > 150;
                                    let pill = egui::Rect::from_min_size(pos2(x, rect.top() + l2), vec2(*w, 15.0));
                                    p.rect_filled(pill, 7.0, fill);
                                    p.text(
                                        pill.center(),
                                        egui::Align2::CENTER_CENTER,
                                        &c.name,
                                        regular(10.5),
                                        if light { egui::Color32::BLACK } else { egui::Color32::WHITE },
                                    );
                                    x = pill.right() + 4.0;
                                }
                                if more > 0 {
                                    p.text(pos2(x, rect.top() + l2), egui::Align2::LEFT_TOP, format!("+{more}"), regular(10.5), t.text_dim);
                                }
                            }
                            if flag_inline {
                                draw_flag(p, pos2(right_base - 10.0, rect.top() + l2));
                            }
                            if show_folder {
                                p.text(
                                    pos2(right_base - if flag_inline { 18.0 } else { 0.0 }, rect.top() + l2),
                                    egui::Align2::RIGHT_TOP,
                                    folder_label(&m.folder, &self.folders),
                                    regular(11.0),
                                    t.text_dim,
                                );
                            }
                        }
                        // One row: a cell per column, as the headings have them (Betterbird's list).
                        if compact {
                            use settings::ListCol as C;
                            let cells = list_cells(rect.left(), rect.right(), &extra, &widths);
                            let shown = |c: C| extra.contains(&c);
                            let cy = rect.center().y;
                            let in_junk = self.kind_of(&m.folder) == foldertree::Kind::Junk;
                            let fmt = {
                                let who = format!("{} {}", m.from_name.as_deref().unwrap_or(""), m.from_addr.as_deref().unwrap_or(""));
                                self.settings.formats.iter().find(|f| f.matches(&who, m.subject.as_deref().unwrap_or(""), unread))
                            };
                            let fmt_col = fmt.map(|f| egui::Color32::from_rgb(f.colour[0], f.colour[1], f.colour[2]));
                            let bold = unread || fmt.is_some_and(|f| f.bold);
                            let font = if bold { semibold(12.5) } else { regular(12.5) };
                            let has_flag = all.iter().any(|x| flagged(x));
                            let files = self.line_files(line);
                            let mut cats: Vec<&accounts::Category> = vec![];
                            for x in &all {
                                for c in self.row_categories(x) {
                                    if !cats.iter().any(|k| k.key == c.key) {
                                        cats.push(c);
                                    }
                                }
                            }
                            let who = |x: &MessageRow| -> String {
                                match (&x.from_name, &x.from_addr) {
                                    (Some(n), Some(a)) if self.settings.list_addresses => format!("{n} <{a}>"),
                                    (n, a) => n.clone().or(a.clone()).unwrap_or_else(|| "(unknown sender)".into()),
                                }
                            };
                            // Where the conversation arrow goes: its own column, else the first text one.
                            let twisty_in = if shown(C::Thread) {
                                Some(C::Thread)
                            } else if shown(C::From) && extra.iter().position(|c| *c == C::From) < extra.iter().position(|c| *c == C::Subject) {
                                Some(C::From)
                            } else {
                                None
                            };
                            for cell in &cells {
                                let cr = egui::Rect::from_min_max(pos2(cell.x0, rect.top()), pos2(cell.x1, rect.bottom()));
                                let clip = ui.painter().with_clip_rect(cr.shrink2(vec2(3.0, 0.0)));
                                let text_at = |s: String, colour: egui::Color32, font: egui::FontId| {
                                    if right_aligned(cell.col) {
                                        clip.text(pos2(cr.right() - 8.0, cy), egui::Align2::RIGHT_CENTER, s, font, colour)
                                    } else {
                                        clip.text(pos2(cr.left() + 6.0, cy), egui::Align2::LEFT_CENTER, s, font, colour)
                                    }
                                };
                                match cell.col {
                                    None => {
                                        // Subject, the preview after it, and at its end what has no column.
                                        let mut x = cr.left() + 6.0;
                                        if twisty_in.is_none() && is_head {
                                            foldertree::twisty(&clip, pos2(x + 4.0, cy), open_head, t.text_dim);
                                            x += 14.0;
                                        }
                                        if !shown(C::From) {
                                            x += indent * 0.5;
                                        }
                                        let mut r = cr.right() - 6.0;
                                        if has_flag && !shown(C::Flag) {
                                            r -= 14.0;
                                            draw_flag(&clip, pos2(r, cy - 7.0));
                                            r -= 4.0;
                                        }
                                        if !files.is_empty() && !shown(C::Attachment) {
                                            r -= 12.0;
                                            draw_clip(&clip, pos2(r, cy - 7.0), t.text_dim);
                                            r -= 4.0;
                                        }
                                        if spans_folders && !shown(C::Location) {
                                            let g = clip.text(
                                                pos2(r, cy),
                                                egui::Align2::RIGHT_CENTER,
                                                folder_label(&m.folder, &self.folders),
                                                regular(11.0),
                                                t.text_dim,
                                            );
                                            r = g.left() - 8.0;
                                        }
                                        let mut pills: Vec<(egui::Rect, &accounts::Category)> = vec![];
                                        if !shown(C::Tag) {
                                            for c in cats.iter().take(3) {
                                                let w = clip.layout_no_wrap(c.name.clone(), regular(10.5), t.text).size().x + 12.0;
                                                if r - w < x + 120.0 {
                                                    break;
                                                }
                                                pills.push((egui::Rect::from_min_size(pos2(r - w, cy - 7.5), vec2(w, 15.0)), c));
                                                r -= w + 4.0;
                                            }
                                        }
                                        let words = clip.with_clip_rect(egui::Rect::from_min_max(pos2(cr.left(), cr.top()), pos2(r, cr.bottom())));
                                        let subj = m.subject.clone().unwrap_or_else(|| "(no subject)".into());
                                        let colour = fmt_col.unwrap_or(if unread { t.blue } else { t.text });
                                        let end = words.text(pos2(x, cy), egui::Align2::LEFT_CENTER, subj, font.clone(), colour);
                                        let mut after = end.right();
                                        if is_head && !shown(C::From) && !shown(C::Total) {
                                            after = words
                                                .text(
                                                    pos2(after + 6.0, cy),
                                                    egui::Align2::LEFT_CENTER,
                                                    format!("({})", all.len()),
                                                    regular(12.0),
                                                    t.text_dim,
                                                )
                                                .right();
                                        }
                                        if self.settings.preview != settings::Preview::Off
                                            && let Some(pv) = m.preview.as_deref().filter(|p| !p.is_empty())
                                        {
                                            words.text(pos2(after + 8.0, cy), egui::Align2::LEFT_CENTER, pv, regular(12.0), t.text_dim);
                                        }
                                        for (pill, c) in pills {
                                            let [r, g, b] = c.colour;
                                            let light = (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000 > 150;
                                            clip.rect_filled(pill, 7.0, egui::Color32::from_rgb(r, g, b));
                                            let ink = if light { egui::Color32::BLACK } else { egui::Color32::WHITE };
                                            clip.text(pill.center(), egui::Align2::CENTER_CENTER, &c.name, regular(10.5), ink);
                                        }
                                    }
                                    Some(C::From) => {
                                        let mut x = cr.left() + 6.0 + indent * 0.5;
                                        if twisty_in == Some(C::From) && is_head {
                                            foldertree::twisty(&clip, pos2(x + 4.0, cy), open_head, t.text_dim);
                                            x += 14.0;
                                        }
                                        if self.settings.sender_images {
                                            let name = m.from_name.clone().or(m.from_addr.clone()).unwrap_or_default();
                                            let c = pos2(x + 10.0, cy);
                                            clip.circle_filled(c, 10.0, sender_colour(m.from_addr.as_deref().unwrap_or(&name)));
                                            clip.text(c, egui::Align2::CENTER_CENTER, initials(&name), semibold(9.0), egui::Color32::WHITE);
                                            x += 26.0;
                                        }
                                        let name = if is_head {
                                            let mut names: Vec<String> = vec![];
                                            for x in &all {
                                                let n = x.from_name.clone().or(x.from_addr.clone()).unwrap_or_default();
                                                if !n.is_empty() && !names.contains(&n) {
                                                    names.push(n);
                                                }
                                            }
                                            names.into_iter().take(3).collect::<Vec<_>>().join("; ")
                                        } else {
                                            who(m)
                                        };
                                        let end = clip.text(pos2(x, cy), egui::Align2::LEFT_CENTER, name, font.clone(), fmt_col.unwrap_or(t.text));
                                        if is_head && !shown(C::Total) {
                                            clip.text(
                                                pos2(end.right() + 6.0, cy),
                                                egui::Align2::LEFT_CENTER,
                                                format!("({})", all.len()),
                                                regular(12.0),
                                                t.text_dim,
                                            );
                                        }
                                    }
                                    Some(C::Recipient) => {
                                        text_at(m.to.clone().unwrap_or_default(), t.text, regular(12.0));
                                    }
                                    Some(C::Correspondents) => {
                                        // What was sent shows who it went to.
                                        let sent = self.kind_of(&m.folder) == foldertree::Kind::Sent;
                                        let s = if sent { format!("To: {}", m.to.clone().unwrap_or_default()) } else { who(m) };
                                        text_at(s, fmt_col.unwrap_or(t.text), font.clone());
                                    }
                                    Some(C::Date) => {
                                        text_at(short_date(&m.date), t.text_dim, regular(11.5));
                                    }
                                    Some(C::Received) => {
                                        text_at(short_date(&m.received.clone().or(m.date.clone())), t.text_dim, regular(11.5));
                                    }
                                    Some(C::Status) => {
                                        text_at(status_text(m).to_string(), t.text_dim, regular(11.5));
                                    }
                                    Some(C::Size) => {
                                        let bytes: i64 = all.iter().map(|x| x.size.unwrap_or(0) as i64).sum();
                                        text_at(size_text(bytes), t.text_dim, regular(11.5));
                                    }
                                    Some(C::Tag) => {
                                        let mut x = cr.left() + 6.0;
                                        for c in &cats {
                                            let [r, g, b] = c.colour;
                                            let w = clip.layout_no_wrap(c.name.clone(), regular(10.5), t.text).size().x + 12.0;
                                            let pill = egui::Rect::from_min_size(pos2(x, cy - 7.5), vec2(w, 15.0));
                                            let light = (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000 > 150;
                                            clip.rect_filled(pill, 7.0, egui::Color32::from_rgb(r, g, b));
                                            clip.text(
                                                pill.center(),
                                                egui::Align2::CENTER_CENTER,
                                                &c.name,
                                                regular(10.5),
                                                if light { egui::Color32::BLACK } else { egui::Color32::WHITE },
                                            );
                                            x = pill.right() + 4.0;
                                        }
                                    }
                                    Some(C::Account) => {
                                        text_at(self.current().map(|a| a.email.clone()).unwrap_or_default(), t.text_dim, regular(11.5));
                                    }
                                    Some(C::Priority) => {
                                        let (s, c) = match m.priority {
                                            Some(1) => ("Highest", t.red),
                                            Some(2) => ("High", t.red),
                                            Some(4) => ("Low", t.text_dim),
                                            Some(5) => ("Lowest", t.text_dim),
                                            _ => ("", t.text_dim),
                                        };
                                        text_at(s.into(), c, regular(11.5));
                                    }
                                    Some(C::Unread) => {
                                        // Conversations only, as Betterbird counts them.
                                        let n = all.iter().filter(|x| !x.seen()).count();
                                        if is_head && n > 0 {
                                            text_at(n.to_string(), t.blue, semibold(11.5));
                                        }
                                    }
                                    Some(C::Total) => {
                                        if is_head {
                                            text_at(all.len().to_string(), t.text_dim, regular(11.5));
                                        }
                                    }
                                    Some(C::Location) => {
                                        text_at(folder_label(&m.folder, &self.folders), t.text_dim, regular(11.5));
                                    }
                                    Some(C::Order) => {
                                        text_at(m.uid.to_string(), t.text_dim, regular(11.5));
                                    }
                                    Some(c) => {
                                        // The icon columns: each a button for its line.
                                        let r = egui::Rect::from_center_size(cr.center(), vec2(22.0, 22.0));
                                        let clickable = !(c == C::Attachment || (c == C::Thread && !is_head));
                                        let bresp = clickable.then(|| ui.interact(r, ui.id().with(("colbtn", li, c as u8)), Sense::click()));
                                        let hot = bresp.as_ref().is_some_and(|r| r.hovered());
                                        if hot {
                                            clip.rect_filled(r, 3.0, t.hover);
                                        }
                                        let tip = match c {
                                            C::Select => {
                                                tick_box(&clip, r.center(), sel && (self.picked.len() > 1 || self.head_selected().is_none()), t);
                                                "Select"
                                            }
                                            C::Thread => {
                                                if is_head {
                                                    foldertree::twisty(&clip, r.center(), open_head, t.text_dim);
                                                }
                                                if open_head { "Close the conversation" } else { "Open the conversation" }
                                            }
                                            C::Flag => {
                                                if has_flag {
                                                    draw_flag(&clip, r.center() + vec2(-5.0, -7.0));
                                                } else {
                                                    let s = egui::Stroke::new(1.0, t.text_dim.gamma_multiply(0.45));
                                                    let o = r.center() + vec2(-4.0, -7.0);
                                                    clip.line_segment([o, o + vec2(0.0, 13.0)], s);
                                                    clip.add(egui::Shape::closed_line(
                                                        vec![o, o + vec2(9.0, 0.0), o + vec2(7.0, 3.0), o + vec2(9.0, 6.0), o + vec2(0.0, 6.0)],
                                                        s,
                                                    ));
                                                }
                                                "Flag / unflag"
                                            }
                                            C::Attachment => {
                                                if !files.is_empty() {
                                                    draw_clip(&clip, r.center() + vec2(-5.0, -7.0), t.text_dim);
                                                }
                                                ""
                                            }
                                            C::Read => {
                                                if unread {
                                                    clip.circle_filled(r.center(), 4.0, t.accent);
                                                } else {
                                                    clip.circle_stroke(r.center(), 3.0, egui::Stroke::new(1.0, t.text_dim.gamma_multiply(0.6)));
                                                }
                                                "Mark as read / unread"
                                            }
                                            C::Delete => {
                                                // Red under the mouse: it deletes.
                                                let ir = egui::Rect::from_center_size(r.center(), vec2(16.0, 16.0));
                                                let ink =
                                                    if hot { egui::Color32::from_rgb(0xE8, 0x3B, 0x3B) } else { t.text_dim.gamma_multiply(0.7) };
                                                foldertree::icon(&clip, ir, Some(foldertree::Kind::Trash), ink, t.accent);
                                                "Delete"
                                            }
                                            C::Junk => {
                                                let ir = egui::Rect::from_center_size(r.center(), vec2(16.0, 16.0));
                                                let tint = if in_junk { None } else { Some(t.text_dim.gamma_multiply(0.45)) };
                                                foldertree::icon_tinted(&clip, ir, Some(foldertree::Kind::Junk), t.text_dim, t.accent, tint);
                                                if in_junk { "Not junk" } else { "Junk" }
                                            }
                                            _ => "",
                                        };
                                        if let Some(b) = bresp
                                            && b.on_hover_text(tip).clicked()
                                        {
                                            col_hit = Some((self.line_items_here(line), c, li, in_junk));
                                        }
                                    }
                                }
                            }
                            ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], egui::Stroke::new(1.0, t.border));
                        }
                        // Quick buttons while the mouse is over the message (Settings > Customise actions).
                        // Not in the one-row list: its columns do these, and the buttons would hide the date.
                        if !compact
                            && !self.settings.hover_actions.is_empty()
                            && ui.rect_contains_pointer(rect)
                            && !egui::DragAndDrop::has_any_payload(ui.ctx())
                        {
                            let n = self.settings.hover_actions.len() as f32;
                            let strip = egui::Rect::from_min_size(pos2(rect.right() - 8.0 - n * 24.0, rect.top() + l1 - 3.0), vec2(n * 24.0, 20.0));
                            ui.painter().rect_filled(strip.expand(2.0), 4.0, bg);
                            for (k, a) in self.settings.hover_actions.iter().enumerate() {
                                let b = egui::Rect::from_min_size(pos2(strip.left() + k as f32 * 24.0, strip.top()), vec2(22.0, 20.0));
                                let r = ui.interact(b, ui.id().with(("hover", li, k)), Sense::click());
                                if r.hovered() {
                                    ui.painter().rect_filled(b, 3.0, t.hover);
                                }
                                let icon = egui::Rect::from_center_size(b.center(), vec2(16.0, 16.0));
                                let tip = match a {
                                    settings::HoverAction::Delete => {
                                        let ink = if r.hovered() { egui::Color32::from_rgb(0xE8, 0x3B, 0x3B) } else { t.text };
                                        foldertree::icon(ui.painter(), icon, Some(foldertree::Kind::Trash), ink, t.accent);
                                        "Delete"
                                    }
                                    settings::HoverAction::Archive => {
                                        foldertree::icon(ui.painter(), icon, Some(foldertree::Kind::Archive), t.text, t.accent);
                                        "Archive"
                                    }
                                    settings::HoverAction::Flag => {
                                        draw_flag(ui.painter(), icon.min + vec2(3.0, 1.0));
                                        "Flag / unflag"
                                    }
                                    settings::HoverAction::ReadUnread => {
                                        foldertree::icon(ui.painter(), icon, None, t.text, t.accent);
                                        "Mark as read / unread"
                                    }
                                };
                                if r.on_hover_text(tip).clicked() {
                                    hover_hit = Some((self.line_items_here(line), *a));
                                }
                            }
                        }
                        if resp.drag_started() {
                            // Dragging a selected line takes the whole selection along.
                            let items = if sel { self.targets() } else { self.line_items_here(line) };
                            resp.dnd_set_drag_payload(DragMail(items));
                        }
                        if resp.double_clicked() {
                            double = Some(li);
                        }
                        if resp.clicked() {
                            let mods = ui.input(|i| i.modifiers);
                            let on_arrow = is_head && resp.interact_pointer_pos().is_some_and(|pos| pos.x < rect.left() + 22.0);
                            hit = Some((
                                li,
                                if on_arrow {
                                    3
                                } else if mods.command {
                                    1
                                } else if mods.shift {
                                    2
                                } else {
                                    0
                                },
                            ));
                        } else if resp.secondary_clicked() && !sel {
                            // Right-click outside the selection selects that line alone.
                            hit = Some((li, 0));
                        }
                        resp.context_menu(|ui| {
                            if let Some(a) = menu(ui, MESSAGE_MENU, &|a| self.state(a)) {
                                menu_action = Some(a);
                            }
                        });
                    }
                });
                if let Some((items, c, li, flag)) = col_hit {
                    match c {
                        settings::ListCol::Select if li == usize::MAX => {
                            // The heading's box: everything, or nothing.
                            if flag {
                                self.picked.clear();
                                self.head = None;
                            } else {
                                self.select_all();
                            }
                        }
                        settings::ListCol::Select => self.click_line(li, 1),
                        settings::ListCol::Thread => self.click_line(li, 3),
                        settings::ListCol::Flag => self.toggle_flag_items(&items),
                        settings::ListCol::Read => self.toggle_read_items(&items),
                        settings::ListCol::Delete => self.delete_items(items),
                        settings::ListCol::Junk => self.move_special_items(
                            items,
                            if flag { mailcraft_core::manage::Special::Inbox } else { mailcraft_core::manage::Special::Junk },
                        ),
                        _ => {}
                    }
                } else if let Some((items, a)) = hover_hit {
                    match a {
                        settings::HoverAction::Delete => self.delete_items(items),
                        settings::HoverAction::Archive => self.move_special_items(items, mailcraft_core::manage::Special::Archive),
                        settings::HoverAction::Flag => self.toggle_flag_items(&items),
                        settings::HoverAction::ReadUnread => self.toggle_read_items(&items),
                    }
                } else if let Some((li, how)) = hit {
                    self.click_line(li, how);
                } else if let Some((group, add)) = day_pick {
                    let mut set = if add { self.targets() } else { vec![] };
                    for x in group {
                        if !set.contains(&x) {
                            set.push(x);
                        }
                    }
                    let focus = set.first().cloned();
                    self.set_picked(set, focus);
                }
                if let Some(li) = double {
                    self.open_in_tab(li);
                }
                if let Some(a) = menu_action {
                    let ctx = ui.ctx().clone();
                    self.act(a, &ctx);
                }
            }
        }
    }

    fn reading_pane(&mut self, ui: &mut egui::Ui, t: &Tokens, frame: &eframe::Frame) {
        // The browser view is a native window over ours: hide it whenever something else needs the space.
        let covered = self.compose.is_some()
            || self.add.is_some()
            || self.prompt.is_some()
            || self.moving.is_some()
            || self.dialog.is_some()
            || self.picked.len() > 1
            || egui::Popup::is_any_open(ui.ctx())
            || self.open.is_none();
        #[cfg(any(windows, target_os = "linux"))]
        {
            if covered && let Some(w) = self.web.as_mut() {
                w.hide();
            }
            if !covered && self.web.is_none() && self.web_error.is_none() {
                match htmlview::HtmlView::new(frame, ui.ctx().clone()) {
                    Ok(w) => self.web = Some(w),
                    Err(e) => {
                        self.status = format!("Showing mail with the built-in renderer (the browser view did not start: {e}).");
                        self.web_error = Some(e);
                    }
                }
            }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = (frame, covered);
        if self.compose.is_some() {
            self.compose_pane(ui, t);
            return;
        }
        let css = self.message_css();
        let dark = self.dark_reading();
        let receipt = self.receipt.clone();
        let mut receipt_choice: Option<bool> = None;
        let invite_cals = if self.open.as_ref().is_some_and(|o| o.invite.is_some()) { self.invite_calendars() } else { vec![] };
        let me = self.current().map(|a| a.email.clone()).unwrap_or_default();
        let mut invite_action: Option<invite::Action> = None;
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| {
            if self.picked.len() > 1 {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new(format!("{} messages selected", self.picked.len())).font(semibold(16.0)).color(t.text_dim));
                });
                return;
            }
            let Some(open) = self.open.as_mut() else {
                ui.centered_and_justified(|ui| {
                    let msg = if self.loading.is_some() {
                        "Opening..."
                    } else if self.messages.is_empty() {
                        "Nothing here yet."
                    } else {
                        "Select a message to read it."
                    };
                    ui.label(RichText::new(msg).font(regular(14.0)).color(t.text_dim));
                });
                return;
            };
            egui::Frame::NONE.fill(t.ribbon).inner_margin(egui::Margin::symmetric(18, 12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&open.subject).font(semibold(18.0)).color(t.text));
                ui.add_space(6.0);
                ui.label(RichText::new(&open.from).font(semibold(13.0)).color(t.text));
                ui.label(RichText::new(format!("To: {}", open.to)).font(regular(12.0)).color(t.text_dim));
                ui.label(RichText::new(&open.date).font(regular(12.0)).color(t.text_dim));
                if open.remote && !open.show_remote {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Pictures from the internet are blocked, to stop senders tracking when you read this.")
                                .font(regular(12.0))
                                .color(t.text_dim),
                        );
                        if ui.button("Show pictures").clicked() {
                            open.show_remote = true;
                            open.prepared = htmlview::prepare(&reading_html(&format!("{}{}", open.html, open.thread), dark), &open.inline, true, css);
                        }
                    });
                }
                if let Some((to, _, _)) = &receipt {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("The sender asked to be told when you read this ({to}).")).font(regular(12.0)).color(t.text_dim),
                        );
                        if ui.button("Send receipt").clicked() {
                            receipt_choice = Some(true);
                        }
                        if ui.button("Don't send").clicked() {
                            receipt_choice = Some(false);
                        }
                    });
                }
                if let Some(inv) = open.invite.as_mut() {
                    ui.add_space(6.0);
                    invite_action = invite::card(ui, t, inv, &invite_cals, &me);
                }
                if !open.attachments.is_empty() {
                    ui.add_space(4.0);
                    let mut note = None;
                    ui.horizontal_wrapped(|ui| {
                        for (name, _, bytes) in &open.attachments {
                            ui.label(RichText::new(format!("📎 {name} ({} KB)", bytes.len().div_ceil(1024))).font(regular(12.0)).color(t.text));
                            if ui.small_button("Open").clicked() {
                                note = Some(match open_attachment(name, bytes) {
                                    Ok(()) => format!("Opened {name}."),
                                    Err(e) => e,
                                });
                            }
                            if ui.small_button("Save…").clicked() {
                                note = Some(match save_attachment(name, bytes) {
                                    Ok(Some(p)) => format!("Saved {p}."),
                                    Ok(None) => String::new(),
                                    Err(e) => format!("Could not save {name}: {e}"),
                                });
                            }
                            ui.add_space(10.0);
                        }
                    });
                    if let Some(n) = note.filter(|n| !n.is_empty()) {
                        self.status = n;
                    }
                }
            });
            #[cfg(any(windows, target_os = "linux"))]
            if let Some(w) = self.web.as_mut() {
                let r = ui.available_rect_before_wrap();
                ui.allocate_rect(r, Sense::hover());
                // While a dialog is up the view stays hidden, or it would sit on top of the dialog.
                if !covered {
                    let ppp = ui.ctx().pixels_per_point();
                    w.show(&open.prepared, r.min.x * ppp, r.min.y * ppp, r.width() * ppp, r.height() * ppp);
                }
                return;
            }
            // No browser view: WordCraft's own renderer (simpler layouts only).
            open.reader.session.view.web_width = ((ui.available_width() - 40.0) / wordcraft_ui_egui::canvas::PX_PER_PT).max(200.0);
            wordcraft_ui_egui::canvas::show(&mut open.reader, ui);
        });
        if let Some(act) = invite_action {
            self.invite_act(act);
        }
        match receipt_choice {
            Some(true) => {
                if let Some(r) = self.receipt.take() {
                    self.send_receipt(r);
                }
            }
            Some(false) => self.receipt = None,
            None => {}
        }
    }

    fn start_compose(&mut self, kind: &str) {
        // The account's signature (Settings > Account), as HTML.
        let sig = self.current().and_then(|a| self.settings.signature_for(&a.email)).cloned();
        let sig_html = |s: &settings::Signature| format!("<p>{}</p>", escape_html(s.text.trim_end()).replace('\n', "<br>"));
        let sig_new = sig.as_ref().filter(|s| s.new_messages).map(sig_html);
        let sig_reply = sig.as_ref().filter(|s| s.replies).map(sig_html).unwrap_or_default();
        let email = self.current().map(|a| a.email.clone()).unwrap_or_default();
        let me = email.to_ascii_lowercase();
        // Send-only addresses this account may write from.
        let mine: Vec<String> = self.my_senders().iter().map(|s| s.email.to_ascii_lowercase()).collect();
        // Replying to mail sent to one of them: reply from it, as Outlook does; a draft keeps its own.
        let from = match (kind, &self.open) {
            ("reply" | "all" | "forward" | "attach", Some(o)) => {
                o.to_list.iter().chain(&o.cc_list).map(|a| a.to_ascii_lowercase()).find(|a| mine.contains(a))
            }
            ("draft", Some(o)) => mine.iter().find(|m| o.from.to_ascii_lowercase().contains(m.as_str())).cloned(),
            _ => None,
        };
        let mut c = match (kind, &self.open) {
            ("reply" | "all", Some(o)) => {
                let mut cc: Vec<String> = vec![];
                if kind == "all" {
                    for a in o.to_list.iter().chain(&o.cc_list) {
                        if a.to_ascii_lowercase() != me
                            && !mine.contains(&a.to_ascii_lowercase())
                            && !o.reply_to.iter().any(|r| r.eq_ignore_ascii_case(a))
                            && !cc.iter().any(|c| c.eq_ignore_ascii_case(a))
                        {
                            cc.push(a.clone());
                        }
                    }
                }
                let quote = format!("<p>On {}, {} wrote:</p><blockquote>{}</blockquote>", escape_html(&o.date), escape_html(&o.from), o.html);
                Compose {
                    to: o.reply_to.join(", "),
                    cc: cc.join(", "),
                    subject: with_prefix("Re:", &o.subject),
                    in_reply_to: o.message_id.clone(),
                    references: o.references.clone(),
                    replied: Some((o.folder.clone(), o.uid)),
                    ..Compose::blank(if kind == "all" { "Reply all" } else { "Reply" }, editor(&format!("{sig_reply}{quote}"), 2), &email)
                }
            }
            ("forward", Some(o)) => {
                let head = format!(
                    "<p>---------- Forwarded message ----------<br>From: {}<br>Date: {}<br>Subject: {}<br>To: {}</p>",
                    escape_html(&o.from),
                    escape_html(&o.date),
                    escape_html(&o.subject),
                    escape_html(&o.to)
                );
                Compose {
                    subject: with_prefix("Fwd:", &o.subject),
                    attachments: o.attachments.clone(),
                    ..Compose::blank("Forward", editor(&format!("{sig_reply}{head}{}", o.html), 2), &email)
                }
            }
            ("attach", Some(o)) => {
                // The whole message, as received, attached to a new one.
                let raw = self.selected_raw().unwrap_or_default();
                Compose {
                    subject: with_prefix("Fwd:", &o.subject),
                    attachments: vec![(format!("{}.eml", safe_file_name(&o.subject)), "message/rfc822".into(), raw)],
                    ..Compose::blank("Forward", if sig_reply.is_empty() { editor("<p></p>", 0) } else { editor(&sig_reply, 2) }, &email)
                }
            }
            ("draft", Some(o)) => {
                // Carry on writing a saved draft: same Message-ID, so the next save replaces it.
                let mut references = o.references.clone();
                if references.last().is_some_and(|r| Some(r) == o.message_id.as_ref()) {
                    references.pop();
                }
                let mut c = Compose {
                    to: o.to_list.join(", "),
                    cc: o.cc_list.join(", "),
                    subject: if o.subject == "(no subject)" { String::new() } else { o.subject.clone() },
                    in_reply_to: o.in_reply_to.clone(),
                    references,
                    attachments: o.attachments.clone(),
                    ..Compose::blank("Draft", editor(&o.html, 0), &email)
                };
                if let Some(id) = &o.message_id {
                    c.draft_id = id.trim_matches(['<', '>']).to_string();
                    c.on_server = true;
                }
                c.saved = c.fingerprint();
                c
            }
            _ => match &sig_new {
                Some(sig) => Compose::blank("New message", editor(sig, 2), &email),
                None => Compose::blank("New message", editor("<p></p>", 0), &email),
            },
        };
        c.from = from;
        if kind == "draft" {
            // The copy on screen is about to be replaced by the next save.
            self.open = None;
            self.selected = None;
        }
        self.compose = Some(c);
    }

    /// Later launches (mailto.rs): bring the window forward, and open the next waiting email
    /// link as a new message once nothing else is being written.
    fn take_links(&mut self, ctx: &egui::Context) {
        let mut arrived = false;
        while let Some((l, done)) = self.links.as_ref().and_then(|r| r.try_recv().ok()) {
            let _ = done.send(());
            if l != mailto::SHOW {
                self.pending_links.push(l);
            }
            arrived = true;
        }
        if arrived {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
            self.alert = true;
        }
        if self.pending_links.is_empty() || self.styled < 2 {
            return;
        }
        if self.compose.is_some() {
            if !self.link_waiting {
                self.link_waiting = true;
                self.status = "An email link is waiting: it opens when the message you are writing is sent or closed.".into();
            }
            return;
        }
        if std::mem::take(&mut self.link_waiting) {
            self.status = "The email link that was waiting is open as a new message.".into();
        }
        let link = self.pending_links.remove(0);
        let Some(m) = mailto::parse(&link) else {
            self.status = "That email link could not be read.".into();
            return;
        };
        self.start_compose("new");
        let sig = self
            .current()
            .and_then(|a| self.settings.signature_for(&a.email))
            .filter(|s| s.new_messages)
            .map(|s| format!("<p>{}</p>", escape_html(s.text.trim_end()).replace('\n', "<br>")))
            .unwrap_or_default();
        if let Some(c) = self.compose.as_mut() {
            c.to = m.to.join(", ");
            c.cc = m.cc.join(", ");
            c.bcc = m.bcc.join(", ");
            c.show_bcc |= !m.bcc.is_empty();
            c.subject = m.subject;
            if !m.body.is_empty() {
                c.editor = editor(&format!("{}{sig}", mailto::body_html(&m.body)), 0);
            }
            c.saved = c.fingerprint();
        }
    }

    /// The send-only addresses offered when writing from the current account.
    fn my_senders(&self) -> Vec<&accounts::Sender> {
        let me = self.current().map(|a| a.email.as_str()).unwrap_or("");
        self.senders.iter().filter(|s| s.account.is_empty() || s.account.eq_ignore_ascii_case(me)).collect()
    }

    fn compose_pane(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let templates = self.settings.templates.clone();
        let parts = self.settings.quick_parts.clone();
        let mut manage: Option<settings_ui::Page> = None;
        let always_bcc = self.settings.always_bcc;
        let plain_text = self.settings.plain_text;
        let warn_mb = self.settings.attach_warn_mb.max(1) as usize;
        let senders: Vec<(String, String)> = self.my_senders().iter().map(|s| (s.email.clone(), s.label())).collect();
        let account_label = match self.current() {
            Some(a) => match self.settings.name_for(&a.email) {
                Some(n) => format!("{n} <{}>", a.email),
                None => a.email.clone(),
            },
            None => String::new(),
        };
        // Suggested people (Settings > Privacy and data).
        let who: &[people::Person] = if self.settings.suggest_people { &self.people } else { &[] };
        let hidden = &self.settings.hidden_people;
        let mut hide: Vec<Option<String>> = vec![];
        let mut send = self.compose.as_mut().is_some_and(|c| std::mem::take(&mut c.send_now));
        let mut save = false;
        let mut close = false;
        let mut discard = false;
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| {
            let Some(c) = self.compose.as_mut() else { return };
            egui::Frame::NONE.fill(t.ribbon).inner_margin(egui::Margin::symmetric(18, 10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(c.kind).font(semibold(16.0)).color(t.text));
                    ui.add_space(16.0);
                    if ui
                        .add_enabled(!c.sending, egui::Button::new(RichText::new(if c.sending { "Sending..." } else { "Send" }).font(semibold(13.0))))
                        .clicked()
                    {
                        send = true;
                    }
                    if ui
                        .add_enabled(
                            !c.sending && c.saving.is_none(),
                            egui::Button::new(RichText::new(if c.saving.is_some() { "Saving..." } else { "Save draft" }).font(regular(13.0))),
                        )
                        .clicked()
                    {
                        save = true;
                    }
                    if ui
                        .add_enabled(!c.sending, egui::Button::new(RichText::new("Close").font(regular(13.0))))
                        .on_hover_text("Close, keeping it in Drafts")
                        .clicked()
                    {
                        close = true;
                    }
                    if ui
                        .add_enabled(!c.sending, egui::Button::new(RichText::new("Discard").font(regular(13.0))))
                        .on_hover_text("Close without sending, and delete the saved draft")
                        .clicked()
                    {
                        discard = true;
                    }
                    if let Some(n) = &c.note {
                        ui.add_space(12.0);
                        ui.label(RichText::new(n).font(regular(12.0)).color(t.text_dim));
                    }
                });
                if let Some(e) = &c.error {
                    ui.colored_label(t.red, e);
                }
                if c.ask_attach {
                    ui.horizontal(|ui| {
                        ui.colored_label(t.red, "This message mentions an attachment, but nothing is attached.");
                        if ui.button("Attach files").clicked() {
                            c.ask_attach = false;
                            for p in pick_files() {
                                match read_attachment(&p) {
                                    Ok(a) => c.attachments.push(a),
                                    Err(e) => c.error = Some(format!("Could not attach {e}")),
                                }
                            }
                        }
                        if ui.button("Send anyway").clicked() {
                            c.ask_attach = false;
                            c.attach_ok = true;
                            c.send_now = true;
                        }
                    });
                }
                ui.add_space(6.0);
                let w = ui.available_width() - 70.0;
                egui::Grid::new("compose_fields").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
                    // From: the account, or one of the send-only addresses.
                    if !senders.is_empty() {
                        ui.label("From");
                        let shown = c
                            .from
                            .as_ref()
                            .and_then(|f| senders.iter().find(|(e, _)| e.eq_ignore_ascii_case(f)))
                            .map(|(_, l)| l.clone())
                            .unwrap_or_else(|| account_label.clone());
                        egui::ComboBox::from_id_salt("compose_from").width(w.min(420.0)).selected_text(shown).show_ui(ui, |ui| {
                            ui.selectable_value(&mut c.from, None, account_label.as_str());
                            for (e, l) in &senders {
                                ui.selectable_value(&mut c.from, Some(e.clone()), l.as_str());
                            }
                        });
                        ui.end_row();
                    }
                    ui.label("To");
                    hide.push(people::recipient_field(ui, "to", &mut c.to, who, hidden, w, "name@example.com, ...", t).hide);
                    ui.end_row();
                    ui.horizontal(|ui| {
                        ui.label("Cc");
                        if !c.show_bcc && !always_bcc && ui.small_button("Bcc").on_hover_text("Add Bcc (blind copy)").clicked() {
                            c.show_bcc = true;
                        }
                    });
                    hide.push(people::recipient_field(ui, "cc", &mut c.cc, who, hidden, w, "", t).hide);
                    ui.end_row();
                    if c.show_bcc || always_bcc {
                        ui.label("Bcc");
                        hide.push(people::recipient_field(ui, "bcc", &mut c.bcc, who, hidden, w, "not shown to the other recipients", t).hide);
                        ui.end_row();
                    }
                    ui.label("Subject");
                    ui.add(egui::TextEdit::singleline(&mut c.subject).desired_width(w));
                    ui.end_row();
                });
                let mut remove = None;
                let mut attach = false;
                ui.horizontal_wrapped(|ui| {
                    if ui.button("📎 Attach files").on_hover_text("Or drop files onto the window").clicked() {
                        attach = true;
                    }
                    // Templates (whole messages) and Quick Parts (text where the cursor is), from Settings.
                    ui.menu_button("Templates", |ui| {
                        for tp in &templates {
                            if menu_item(ui, &tp.name) {
                                insert_text(&mut c.editor, &tp.text);
                                if c.subject.trim().is_empty() {
                                    c.subject = tp.subject.clone();
                                }
                                ui.close();
                            }
                        }
                        if !templates.is_empty() {
                            ui.separator();
                        }
                        if menu_item(ui, "Manage templates...") {
                            manage = Some(settings_ui::Page::Templates);
                            ui.close();
                        }
                    });
                    ui.menu_button("Quick Parts", |ui| {
                        for qp in &parts {
                            if menu_item(ui, &qp.name) {
                                insert_text(&mut c.editor, &qp.text);
                                ui.close();
                            }
                        }
                        if !parts.is_empty() {
                            ui.separator();
                        }
                        if menu_item(ui, "Manage Quick Parts...") {
                            manage = Some(settings_ui::Page::QuickParts);
                            ui.close();
                        }
                    });
                    ui.separator();
                    for (i, (name, _, bytes)) in c.attachments.iter().enumerate() {
                        ui.label(RichText::new(format!("{name} ({} KB)", bytes.len().div_ceil(1024))).font(regular(12.0)));
                        if ui.small_button("Remove").clicked() {
                            remove = Some(i);
                        }
                        ui.add_space(8.0);
                    }
                });
                if let Some(i) = remove {
                    c.attachments.remove(i);
                }
                if attach {
                    for p in pick_files() {
                        match read_attachment(&p) {
                            Ok(a) => c.attachments.push(a),
                            Err(e) => c.error = Some(format!("Could not attach {e}")),
                        }
                    }
                }
                let total: usize = c.attachments.iter().map(|a| a.2.len()).sum();
                if total > warn_mb * 1024 * 1024 {
                    ui.colored_label(
                        t.red,
                        format!("Attachments total {} MB; more than the {warn_mb} MB set in Settings > Attachments.", total / (1024 * 1024)),
                    );
                }
                // Format Text: WordCraft's own Font and Paragraph groups, run on the message's
                // editor, so they behave exactly as in WordCraft.
                ui.add_space(6.0);
                let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
                ui.painter().rect_filled(line, 0.0, t.border);
                ui.add_space(4.0);
                egui::ScrollArea::horizontal()
                    .id_salt("compose_format_text")
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                            // Room for the group names under the buttons.
                            ui.set_min_height(wordcraft_ui_egui::widgets::CONTENT_H + wordcraft_ui_egui::widgets::LABEL_H);
                            wordcraft_ui_egui::ribbon::format_text(&mut c.editor, ui);
                            if plain_text {
                                ui.add_space(8.0);
                                ui.label(
                                    RichText::new("Plain text is on (Settings > Compose):\nthis message goes without formatting.")
                                        .font(regular(12.0))
                                        .color(t.text_dim),
                                );
                            }
                        });
                    });
            });
            c.editor.session.view.web_width = ((ui.available_width() - 40.0) / wordcraft_ui_egui::canvas::PX_PER_PT).max(200.0);
            wordcraft_ui_egui::canvas::show(&mut c.editor, ui);
            // The Font and Paragraph dialogs (the groups' launchers, Ctrl+D).
            wordcraft_ui_egui::dialogs::show(&mut c.editor, &ui.ctx().clone());
        });
        if let Some(page) = manage {
            self.set_sel = None;
            self.open_settings(page);
        }
        let hide: Vec<String> = hide.into_iter().flatten().collect();
        if !hide.is_empty() {
            for h in hide {
                if !self.settings.hidden_people.iter().any(|x| x.eq_ignore_ascii_case(&h)) {
                    self.status = format!("{h} won't be suggested again (Settings > Privacy and data).");
                    self.settings.hidden_people.push(h);
                }
            }
            let _ = settings::save(&self.settings);
        }
        if discard {
            if let Some(id) = self.compose.as_ref().and_then(Compose::draft_on_server)
                && let Some(acct) = self.current().cloned()
                && let Some(pw) = secrets::get(&acct.email)
            {
                let _ = self.worker.send(Request::DeleteDraft(acct, pw, id));
                self.status = "Draft discarded.".into();
            }
            self.compose = None;
            return;
        }
        if save {
            self.save_draft();
        }
        if close {
            let changed = self.compose.as_ref().is_some_and(|c| c.saving.is_none() && c.fingerprint() != c.saved);
            if changed {
                self.save_draft();
            }
            if self.compose.as_ref().is_some_and(|c| c.saving.is_some() || c.on_server) {
                self.status = "Kept in Drafts.".into();
            }
            self.compose = None;
            return;
        }
        if send {
            let (Some(acct), Some(c)) = (self.current().cloned(), self.compose.as_mut()) else { return };
            let Some(pw) = secrets::get(&acct.email) else {
                self.prompt = Some((self.account, String::new()));
                return;
            };
            let text = String::from_utf8_lossy(&wordcraft_formats::txt::export(&c.editor.session.doc)).to_string();
            // Outlook's forgotten-attachment check: ask once before sending.
            if self.settings.attach_reminder && c.attachments.is_empty() && !c.attach_ok && settings::mentions_attachment(&c.subject, &text) {
                c.ask_attach = true;
                return;
            }
            let mut out = c.outgoing(&acct, self.settings.name_for(&acct.email), self.settings.plain_text);
            // From a send-only address: its name and address, through its own server.
            let mut via = None;
            if let Some(sender) = c.from.as_ref().and_then(|f| self.senders.iter().find(|s| s.email.eq_ignore_ascii_case(f))) {
                let Some(spw) = secrets::get(&sender.secret_name()) else {
                    c.error = Some(format!("{} is not signed in or has no password saved: Account settings > Send-only addresses.", sender.email));
                    return;
                };
                out.from = sender.email.clone();
                out.from_name = Some(sender.name.clone()).filter(|n| !n.trim().is_empty());
                via = Some(Box::new(worker::Via {
                    endpoint: sender.endpoint(),
                    username: sender.username.clone(),
                    password: spw,
                    oauth: sender.oauth.map(|_| sender.as_account()),
                }));
            }
            c.sending = true;
            c.error = None;
            self.status = "Sending...".into();
            let _ = self.worker.send(Request::Send(acct, pw, Box::new(out), c.replied.clone(), c.draft_on_server(), via));
        }
    }

    /// Save what is being written to the account's Drafts folder (replacing the last save).
    fn save_draft(&mut self) {
        let (Some(acct), Some(c)) = (self.current().cloned(), self.compose.as_mut()) else { return };
        if c.sending || c.saving.is_some() {
            return;
        }
        let Some(pw) = secrets::get(&acct.email) else {
            c.note = Some("Draft not saved: the password is needed (Sync asks for it).".into());
            return;
        };
        let mut out = c.outgoing(&acct, self.settings.name_for(&acct.email), self.settings.plain_text);
        out.message_id = Some(c.draft_id.clone());
        // A draft from a send-only address keeps it, so it opens again from there.
        if let Some(sender) = c.from.as_ref().and_then(|f| self.senders.iter().find(|s| s.email.eq_ignore_ascii_case(f))) {
            out.from = sender.email.clone();
            out.from_name = Some(sender.name.clone()).filter(|n| !n.trim().is_empty());
        }
        c.saving = Some(c.fingerprint());
        c.checked = std::time::Instant::now();
        let _ = self.worker.send(Request::SaveDraft(acct, pw, Box::new(out)));
    }

    /// The account's hierarchy separator ("/" or "."), for building folder names.
    fn delimiter(&self) -> String {
        self.folders.iter().find_map(|f| f.delimiter.clone().filter(|d| !d.is_empty())).unwrap_or_else(|| "/".into())
    }

    /// Categorise (tick categories for the selected messages) and Manage categories.
    fn categories_dialog(&mut self, ctx: &egui::Context) {
        let targets = self.targets();
        // How many of the selected messages have each category.
        let counts: Vec<usize> = self
            .categories
            .iter()
            .map(|c| {
                targets
                    .iter()
                    .filter(|(f, u)| {
                        self.messages.iter().any(|m| m.uid == *u && m.folder == *f && self.row_categories(m).iter().any(|x| x.key == c.key))
                    })
                    .count()
            })
            .collect();
        let Some(d) = self.dialog.as_mut() else { return };
        let mut next: Option<Option<Dialog>> = None;
        let mut apply: Option<(String, bool)> = None;
        let mut save: Option<Vec<accounts::Category>> = None;
        match d {
            Dialog::Categorise => {
                egui::Window::new("Categorise").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(
                    ctx,
                    |ui| {
                        let n = targets.len();
                        ui.label(if n == 1 { "Categories for this message:".to_string() } else { format!("Categories for these {n} messages:") });
                        ui.add_space(4.0);
                        for (c, &have) in self.categories.iter().zip(&counts) {
                            ui.horizontal(|ui| {
                                let (sw, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                                ui.painter().rect_filled(sw, 3.0, egui::Color32::from_rgb(c.colour[0], c.colour[1], c.colour[2]));
                                let mut on = have > 0 && have == n;
                                let some = have > 0 && have < n;
                                if ui.add(egui::Checkbox::new(&mut on, c.name.as_str()).indeterminate(some)).changed() {
                                    // Ticking gives it to all of them; unticking takes it off all.
                                    apply = Some((c.key.clone(), have < n));
                                }
                            });
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("Manage categories...").clicked() {
                                next = Some(Some(Dialog::Categories(self.categories.clone())));
                            }
                            if ui.button("Close").clicked() {
                                next = Some(None);
                            }
                        });
                    },
                );
            }
            Dialog::Categories(list) => {
                egui::Window::new("Categories").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(
                    ctx,
                    |ui| {
                        ui.set_min_width(400.0);
                        ui.label("Click a colour to change it; names can be changed and stay on the messages.");
                        ui.add_space(4.0);
                        let mut remove = None;
                        egui::Grid::new("categories").num_columns(3).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
                            for (i, c) in list.iter_mut().enumerate() {
                                ui.color_edit_button_srgb(&mut c.colour);
                                ui.add_sized([240.0, 22.0], egui::TextEdit::singleline(&mut c.name));
                                if ui.small_button("Delete").clicked() {
                                    remove = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                        if let Some(i) = remove {
                            list.remove(i);
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("New category").clicked() {
                                let mut key = accounts::keyword("New category");
                                let mut n = 2;
                                while list.iter().any(|c| c.key.eq_ignore_ascii_case(&key)) {
                                    key = format!("{}_{n}", accounts::keyword("New category"));
                                    n += 1;
                                }
                                list.push(accounts::Category { name: "New category".into(), colour: [0x00, 0x78, 0xD4], key });
                            }
                            if ui.button("Save").clicked() {
                                for c in list.iter_mut() {
                                    if c.name.trim().is_empty() {
                                        c.name = c.key.replace('_', " ");
                                    }
                                }
                                save = Some(list.clone());
                                next = Some(Some(Dialog::Categorise));
                            }
                            if ui.button("Cancel").clicked() {
                                next = Some(Some(Dialog::Categorise));
                            }
                        });
                    },
                );
            }
            _ => {}
        }
        if let Some(list) = save {
            self.categories = list;
            if let Err(e) = accounts::save_categories(&self.categories) {
                self.status = format!("Could not save the categories: {e}");
            }
        }
        if let Some((key, on)) = apply
            && let Some((acct, pw)) = self.password()
        {
            let _ = self.worker.send(Request::Categorise(acct, pw, by_folder(&targets), key, on));
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && next.is_none() {
            next = Some(None);
        }
        if let Some(n) = next {
            self.dialog = n;
        }
    }

    /// The Rules manager and the rule editor.
    fn rules_dialog(&mut self, ctx: &egui::Context) {
        let account = self.current().map(|a| a.email.clone()).unwrap_or_default();
        let folders: Vec<(String, String)> = foldertree::rows(&self.folders, &Default::default())
            .into_iter()
            .filter_map(|r| r.folder.and_then(|i| self.folders.get(i)).map(|f| (f.name.clone(), format!("{}{}", "    ".repeat(r.depth), r.label))))
            .collect();
        let here = self.folder.clone();
        let cats: Vec<(String, String)> = self.categories.iter().map(|c| (c.key.clone(), c.name.clone())).collect();
        let label_of = |name: &str| folders.iter().find(|(n, _)| n == name).map(|(_, l)| l.trim().to_string()).unwrap_or_else(|| name.to_string());
        let rules = &mut self.rules;
        let Some(d) = self.dialog.as_mut() else { return };
        let mut next: Option<Option<Dialog>> = None;
        let mut save = false;
        let mut run = false;
        let red = egui::Color32::from_rgb(200, 60, 40);
        match d {
            Dialog::Rules => {
                egui::Window::new("Rules").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
                    ui.label(format!("Rules for {account}. They run, top to bottom, on new mail arriving in the Inbox."));
                    ui.add_space(6.0);
                    let mine: Vec<usize> = rules.iter().enumerate().filter(|(_, r)| r.account == account).map(|(i, _)| i).collect();
                    if mine.is_empty() {
                        ui.label(RichText::new("No rules yet.").color(egui::Color32::GRAY));
                    }
                    let mut op: Option<(&str, usize, usize)> = None;
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        egui::Grid::new("rules_list").num_columns(3).spacing(vec2(10.0, 8.0)).striped(true).show(ui, |ui| {
                            for (pos, &i) in mine.iter().enumerate() {
                                let Some(r) = rules.get_mut(i) else { continue };
                                if ui.checkbox(&mut r.enabled, "").on_hover_text("On / off").changed() {
                                    save = true;
                                }
                                ui.vertical(|ui| {
                                    ui.set_max_width(460.0);
                                    ui.label(RichText::new(r.name.as_str()).font(semibold(13.0)));
                                    ui.label(RichText::new(r.summary()).font(regular(11.5)).color(egui::Color32::GRAY));
                                });
                                ui.horizontal(|ui| {
                                    if ui.small_button("Edit").clicked() {
                                        op = Some(("edit", pos, i));
                                    }
                                    if ui.add_enabled(pos > 0, egui::Button::new("Up").small()).clicked() {
                                        op = Some(("up", pos, i));
                                    }
                                    if ui.add_enabled(pos + 1 < mine.len(), egui::Button::new("Down").small()).clicked() {
                                        op = Some(("down", pos, i));
                                    }
                                    if ui.small_button(RichText::new("Delete").color(red)).clicked() {
                                        op = Some(("delete", pos, i));
                                    }
                                });
                                ui.end_row();
                            }
                        });
                    });
                    match op {
                        Some(("edit", _, i)) => {
                            if let Some(r) = rules.get(i) {
                                next = Some(Some(Dialog::RuleEdit { index: Some(i), rule: r.clone() }));
                            }
                        }
                        Some(("up", pos, i)) => {
                            if let Some(&j) = pos.checked_sub(1).and_then(|p| mine.get(p)) {
                                rules.swap(i, j);
                                save = true;
                            }
                        }
                        Some(("down", pos, i)) => {
                            if let Some(&j) = mine.get(pos + 1) {
                                rules.swap(i, j);
                                save = true;
                            }
                        }
                        Some(("delete", _, i)) if i < rules.len() => {
                            rules.remove(i);
                            save = true;
                        }
                        _ => {}
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("New rule...").clicked() {
                            next = Some(Some(Dialog::RuleEdit {
                                index: None,
                                rule: mailcraft_core::rules::Rule { account: account.clone(), ..Default::default() },
                            }));
                        }
                        let any_on = rules.iter().any(|r| r.enabled && r.account == account);
                        let run_label = match &here {
                            Some(f) => format!("Run rules now on {}", label_of(f)),
                            None => "Run rules now".into(),
                        };
                        if ui
                            .add_enabled(any_on && here.is_some(), egui::Button::new(run_label))
                            .on_hover_text("Apply the rules that are on to every message in the open folder")
                            .clicked()
                        {
                            run = true;
                        }
                        if ui.button("Close").clicked() {
                            next = Some(None);
                        }
                    });
                });
            }
            Dialog::RuleEdit { index, rule } => {
                let title = if index.is_some() { "Edit rule" } else { "New rule" };
                egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
                    egui::Grid::new("rule_name").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                        ui.label("Name");
                        ui.add(egui::TextEdit::singleline(&mut rule.name).desired_width(320.0).hint_text("e.g. Receipts"));
                        ui.end_row();
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("When a message arrives and").font(semibold(13.0)));
                        ui.radio_value(&mut rule.match_all, true, "all");
                        ui.radio_value(&mut rule.match_all, false, "any");
                        ui.label(RichText::new("of these are true:").font(semibold(13.0)));
                    });
                    egui::Grid::new("rule_when").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                        ui.label("From contains");
                        ui.add(egui::TextEdit::singleline(&mut rule.from).desired_width(320.0).hint_text("name or address"));
                        ui.end_row();
                        ui.label("To contains");
                        ui.add(egui::TextEdit::singleline(&mut rule.to).desired_width(320.0));
                        ui.end_row();
                        ui.label("Subject contains");
                        ui.add(egui::TextEdit::singleline(&mut rule.subject).desired_width(320.0));
                        ui.end_row();
                        let mut sized = rule.min_size_kb.is_some();
                        ui.checkbox(&mut sized, "Size is at least");
                        let mut kb = rule.min_size_kb.unwrap_or(1024);
                        ui.horizontal(|ui| {
                            ui.add_enabled(sized, egui::DragValue::new(&mut kb).range(1..=1_000_000).speed(10));
                            ui.label("KB");
                        });
                        rule.min_size_kb = sized.then_some(kb);
                        ui.end_row();
                    });
                    ui.label(
                        RichText::new("Several words? Separate them with ; and any of them matches.").font(regular(11.5)).color(egui::Color32::GRAY),
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new("Do this:").font(semibold(13.0)));
                    let first = folders.first().map(|(n, _)| n.clone()).unwrap_or_default();
                    let folder_choice = |ui: &mut egui::Ui, id: &str, label: &str, slot: &mut Option<String>, enabled: bool| {
                        ui.horizontal(|ui| {
                            let mut on = slot.is_some();
                            ui.add_enabled(enabled, egui::Checkbox::new(&mut on, label));
                            if on && enabled {
                                let mut v = slot.clone().unwrap_or_else(|| first.clone());
                                egui::ComboBox::from_id_salt(id).width(240.0).selected_text(label_of(&v)).show_ui(ui, |ui| {
                                    for (n, l) in &folders {
                                        ui.selectable_value(&mut v, n.clone(), l.as_str());
                                    }
                                });
                                *slot = Some(v);
                            } else {
                                *slot = None;
                            }
                        });
                    };
                    let deleting = rule.delete;
                    folder_choice(ui, "rule_move", "Move to folder", &mut rule.move_to, !deleting);
                    folder_choice(ui, "rule_copy", "Copy to folder", &mut rule.copy_to, true);
                    ui.checkbox(&mut rule.mark_read, "Mark as read");
                    ui.checkbox(&mut rule.flag, "Flag");
                    ui.horizontal(|ui| {
                        let mut on = rule.categorise.is_some();
                        ui.checkbox(&mut on, "Categorise");
                        if on {
                            let mut v = rule.categorise.clone().or_else(|| cats.first().map(|(k, _)| k.clone())).unwrap_or_default();
                            let shown = cats.iter().find(|(k, _)| *k == v).map(|(_, n)| n.clone()).unwrap_or_else(|| v.clone());
                            egui::ComboBox::from_id_salt("rule_cat").width(220.0).selected_text(shown).show_ui(ui, |ui| {
                                for (k, n) in &cats {
                                    ui.selectable_value(&mut v, k.clone(), n.as_str());
                                }
                            });
                            rule.categorise = Some(v);
                        } else {
                            rule.categorise = None;
                        }
                    });
                    ui.checkbox(&mut rule.delete, "Delete (move to Trash)");
                    ui.checkbox(&mut rule.stop, "Stop processing more rules");
                    ui.add_space(6.0);
                    let why = if !rule.has_condition() {
                        Some("Give it at least one condition.")
                    } else if !rule.has_action() {
                        Some("Give it at least one thing to do.")
                    } else {
                        None
                    };
                    if let Some(w) = why {
                        ui.colored_label(red, w);
                    }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(why.is_none(), egui::Button::new("Save")).clicked() {
                            let mut r = rule.clone();
                            if r.name.trim().is_empty() {
                                r.name =
                                    [r.from.trim(), r.subject.trim(), r.to.trim()].into_iter().find(|s| !s.is_empty()).unwrap_or("Rule").to_string();
                            }
                            r.account = account.clone();
                            match index.and_then(|i| rules.get_mut(i)) {
                                Some(slot) => *slot = r,
                                None => rules.push(r),
                            }
                            save = true;
                            next = Some(Some(Dialog::Rules));
                        }
                        if ui.button("Cancel").clicked() {
                            next = Some(Some(Dialog::Rules));
                        }
                    });
                });
            }
            _ => {}
        }
        if save && let Err(e) = accounts::save_rules(&self.rules) {
            self.status = format!("Could not save the rules: {e}");
        }
        if run && let (Some(f), Some((acct, pw))) = (here, self.password()) {
            self.status = format!("Running the rules on {f}...");
            let _ = self.worker.send(Request::RunRules(acct, pw, f));
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && next.is_none() {
            next = Some(if matches!(self.dialog, Some(Dialog::RuleEdit { .. })) { Some(Dialog::Rules) } else { None });
        }
        if let Some(n) = next {
            self.dialog = n;
        }
    }

    fn small_dialogs(&mut self, ctx: &egui::Context) {
        let key_set = self.settings.key_set;
        if matches!(self.dialog, Some(Dialog::Rules | Dialog::RuleEdit { .. })) {
            self.rules_dialog(ctx);
            return;
        }
        if matches!(self.dialog, Some(Dialog::Categorise | Dialog::Categories(_))) {
            self.categories_dialog(ctx);
            return;
        }
        if let Some(Dialog::Sweep { sender, folder, choice, days, move_to }) = self.dialog.clone() {
            let (mut choice, mut days, mut move_to) = (choice, days, move_to);
            let folders = self.folder_choices();
            let acct = self.current().map(|a| a.email.clone()).unwrap_or_default();
            let rows = self
                .store
                .as_ref()
                .and_then(|st| st.folder_id(&acct, &folder).ok().flatten().and_then(|id| st.messages(id, 5000).ok()))
                .unwrap_or_default();
            let victims = self.sweep_victims(&rows, &sender, choice == 2, (choice == 3).then_some(days));
            let mut ok = false;
            let mut cancel = false;
            egui::Window::new("Sweep").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
                ui.label(RichText::new(format!("Messages from {sender} in {}", folder_label(&folder, &self.folders))).font(semibold(13.5)));
                ui.add_space(6.0);
                ui.radio_value(&mut choice, 0, "Remove all of them now");
                ui.radio_value(&mut choice, 1, "Remove all of them now, and any that arrive later");
                ui.radio_value(&mut choice, 2, "Always keep only the latest one");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut choice, 3, "Always remove the ones older than");
                    ui.add(egui::DragValue::new(&mut days).range(1..=3650));
                    ui.label("days");
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let mut on = move_to.is_some();
                    ui.checkbox(&mut on, "Move them to a folder instead of deleting");
                    if on {
                        let mut v = move_to.clone().or_else(|| folders.first().map(|(k, _)| k.clone())).unwrap_or_default();
                        let shown = folders.iter().find(|(k, _)| *k == v).map(|(_, n)| n.trim().to_string()).unwrap_or_else(|| v.clone());
                        egui::ComboBox::from_id_salt("sweep_to").width(200.0).selected_text(shown).show_ui(ui, |ui| {
                            for (k, n) in &folders {
                                ui.selectable_value(&mut v, k.clone(), n.as_str());
                            }
                        });
                        move_to = Some(v);
                    } else {
                        move_to = None;
                    }
                });
                ui.add_space(4.0);
                let n = victims.len();
                ui.label(RichText::new(if n == 1 { "1 message now.".to_string() } else { format!("{n} messages now.") }).color(egui::Color32::GRAY));
                ui.horizontal(|ui| {
                    ok = ui.button("OK").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
            if ok {
                match move_to.clone() {
                    Some(to) => self.move_to(victims, to, false),
                    None => self.delete_items(victims),
                }
                match choice {
                    1 => {
                        // Later ones too: a rule, as Outlook's Sweep makes one.
                        self.rules.push(mailcraft_core::rules::Rule {
                            name: format!("Sweep: {sender}"),
                            account: acct.clone(),
                            from: sender.clone(),
                            move_to: move_to.clone(),
                            delete: move_to.is_none(),
                            ..Default::default()
                        });
                        let _ = accounts::save_rules(&self.rules);
                    }
                    2 | 3 => {
                        self.settings.sweeps.push(settings::Sweep {
                            account: acct.clone(),
                            folder: folder.clone(),
                            sender: sender.clone(),
                            keep_latest: choice == 2,
                            older_than_days: (choice == 3).then_some(days),
                            move_to: move_to.clone(),
                        });
                        let _ = settings::save(&self.settings);
                    }
                    _ => {}
                }
                self.dialog = None;
            } else if cancel || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.dialog = None;
            } else {
                self.dialog = Some(Dialog::Sweep { sender, folder, choice, days, move_to });
            }
            return;
        }
        if let Some(Dialog::Conversations { on }) = self.dialog {
            let mut choice: Option<bool> = None;
            let mut cancel = false;
            let folder = self.folder.as_deref().map(|f| folder_label(f, &self.folders)).unwrap_or_default();
            let title = if on { "Show as conversations" } else { "Stop showing as conversations" };
            egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
                ui.label(if on { "Show messages grouped into conversations in:" } else { "Show messages one by one in:" });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(self.folder.is_some(), egui::Button::new(format!("This folder ({folder})"))).clicked() {
                        choice = Some(false);
                    }
                    if ui.button("All folders").clicked() {
                        choice = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
            if let Some(all) = choice {
                if all {
                    // Every folder, as Outlook's "All mailboxes": folder settings give way.
                    self.threads = on;
                    self.conv_folders.clear();
                } else if let (Some(acct), Some(f)) = (self.current().map(|a| a.email.clone()), self.folder.clone()) {
                    self.conv_folders.retain(|(r, _)| !(r.account == acct && r.folder == f));
                    self.conv_folders.push((accounts::FolderRef { account: acct, folder: f }, on));
                }
                self.head = None;
                self.arrange();
            }
            if choice.is_some() || cancel || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.dialog = None;
            }
            return;
        }
        let delim = self.delimiter();
        let taken: Vec<String> = self.folders.iter().map(|f| f.name.to_lowercase()).collect();
        let account_names: Vec<String> = self.accounts.iter().map(|a| a.email.clone()).collect();
        let Some(d) = self.dialog.as_mut() else { return };
        let mut close = false;
        let mut run: Option<FolderOp> = None;
        let mut remove: Option<usize> = None;
        let mut gone: Option<String> = None;
        let title = match d {
            Dialog::NewFolder { .. } => "New folder",
            Dialog::Rename { .. } => "Rename folder",
            Dialog::DeleteFolder(_) => "Delete folder",
            Dialog::Empty(_) => "Empty folder",
            Dialog::RemoveAccount(_) => "Remove account",
            Dialog::Source { .. } => "Message source",
            Dialog::Rules | Dialog::RuleEdit { .. } => "Rules",
            Dialog::Categorise | Dialog::Categories(_) => "Categories",
            Dialog::Conversations { .. } => "Conversations",
            Dialog::Sweep { .. } => "Sweep",
            Dialog::About => "About MailCraft",
            Dialog::Shortcuts => "Keyboard shortcuts",
        };
        let kinds: Vec<(String, foldertree::Kind)> = self.folders.iter().map(|f| (f.name.clone(), foldertree::kind(f))).collect();
        let red = egui::Color32::from_rgb(200, 60, 40);
        // A folder name must be new and must not contain the hierarchy separator.
        let problem = |leaf: &str, full: &str, was: Option<&str>| -> Option<String> {
            if leaf.is_empty() {
                None
            } else if leaf.contains(delim.as_str()) {
                Some(format!("A folder name cannot contain \"{delim}\"."))
            } else if taken.contains(&full.to_lowercase()) && was.is_none_or(|w| !w.eq_ignore_ascii_case(full)) {
                Some("There is already a folder with that name.".into())
            } else {
                None
            }
        };
        egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| match d {
            Dialog::NewFolder { parent, name } => {
                if let Some(p) = parent {
                    ui.label(format!("Inside {p}"));
                }
                ui.label("Folder name");
                let r = ui.add(egui::TextEdit::singleline(name).desired_width(280.0));
                if ui.memory(|m| m.focused().is_none()) {
                    r.request_focus();
                }
                let leaf = name.trim().to_string();
                let full = match parent {
                    Some(p) => format!("{p}{delim}{leaf}"),
                    None => leaf.clone(),
                };
                let why = problem(&leaf, &full, None);
                if let Some(w) = &why {
                    ui.colored_label(red, w);
                }
                let ok = !leaf.is_empty() && why.is_none();
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    if ui.add_enabled(ok, egui::Button::new("Create")).clicked() || (enter && ok) {
                        run = Some(FolderOp::Create(full.clone()));
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
            Dialog::Rename { from, name } => {
                ui.label(format!("New name for {from}"));
                let r = ui.add(egui::TextEdit::singleline(name).desired_width(280.0));
                if ui.memory(|m| m.focused().is_none()) {
                    r.request_focus();
                }
                let leaf = name.trim().to_string();
                let full = match from.rsplit_once(delim.as_str()) {
                    Some((p, _)) => format!("{p}{delim}{leaf}"),
                    None => leaf.clone(),
                };
                let why = problem(&leaf, &full, Some(from.as_str()));
                if let Some(w) = &why {
                    ui.colored_label(red, w);
                }
                let ok = !leaf.is_empty() && why.is_none() && full != *from;
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    if ui.add_enabled(ok, egui::Button::new("Rename")).clicked() || (enter && ok) {
                        run = Some(FolderOp::Rename(from.clone(), full.clone()));
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
            Dialog::DeleteFolder(f) => {
                ui.label(format!("Delete {f}, its subfolders and all the mail in them?"));
                ui.label("This cannot be undone.");
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Delete folder").color(red)).clicked() {
                        run = Some(FolderOp::Delete(f.clone()));
                        gone = Some(f.clone());
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
            Dialog::Empty(f) => {
                let for_good = matches!(kinds.iter().find(|(n, _)| n == f).map(|(_, k)| *k), Some(foldertree::Kind::Trash | foldertree::Kind::Junk));
                if for_good {
                    ui.label(format!("Delete everything in {f} for good?"));
                    ui.label("This cannot be undone.");
                } else {
                    ui.label(format!("Move everything in {f} to Trash?"));
                    ui.label("Its subfolders are left as they are. Messages can be got back from Trash.");
                }
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Empty").color(red)).clicked() {
                        run = Some(if for_good { FolderOp::Empty(f.clone()) } else { FolderOp::EmptyToTrash(f.clone()) });
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
            Dialog::RemoveAccount(i) => {
                let name = account_names.get(*i).cloned().unwrap_or_default();
                ui.label(format!("Remove {name} from MailCraft?"));
                ui.label("The mail stays on the server. MailCraft forgets the account, its saved sign-in and its copy of the mail.");
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Remove account").color(red)).clicked() {
                        remove = Some(*i);
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
            Dialog::Source { title, text } => {
                ui.label(RichText::new(title.as_str()).font(semibold(13.0)));
                egui::ScrollArea::both().max_height(480.0).max_width(780.0).show(ui, |ui| {
                    // Raw mail as it is: long lines scroll sideways rather than wrap.
                    ui.add(egui::Label::new(RichText::new(text.as_str()).monospace().size(12.0)).selectable(true).extend());
                });
                ui.horizontal(|ui| {
                    if ui.button("Copy all").clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            }
            Dialog::Rules | Dialog::RuleEdit { .. } => {}
            Dialog::Categorise | Dialog::Categories(_) => {}
            Dialog::Conversations { .. } => {}
            Dialog::Sweep { .. } => {}
            Dialog::About => {
                ui.label(RichText::new(format!("MailCraft {}", env!("CARGO_PKG_VERSION"))).font(semibold(16.0)));
                ui.label("Email for any account: IMAP and SMTP, Gmail and Outlook.");
                ui.label("Written with WordCraft's engine. Open source, MIT or Apache-2.0.");
                if ui.button("Close").clicked() {
                    close = true;
                }
            }
            Dialog::Shortcuts => {
                egui::Grid::new("shortcuts").num_columns(2).spacing(vec2(24.0, 4.0)).show(ui, |ui| {
                    for (keys, what) in shortcut_list(key_set) {
                        ui.label(RichText::new(keys).font(semibold(12.5)));
                        ui.label(what);
                        ui.end_row();
                    }
                });
                if ui.button("Close").clicked() {
                    close = true;
                }
            }
        });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if let Some(op) = run {
            self.folder_op(op);
        }
        if let Some(f) = gone
            && self.folder.as_deref() == Some(f.as_str())
        {
            self.folder = None;
            self.selected = None;
            self.open = None;
        }
        if let Some(i) = remove {
            self.remove_account(i);
        }
        if close {
            self.dialog = None;
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        self.small_dialogs(ctx);
        let mut close_add = false;
        let signing_in = self.signing_in.clone();
        let mut restart = false;
        let mut outcome: (Option<(Account, String)>, Option<Account>) = (None, None);
        if let Some(d) = self.add.as_mut() {
            let mut save: Option<Account> = None;
            let mut sign_in: Option<Account> = None;
            egui::Window::new("Add an email account").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(
                ctx,
                |ui| {
                    ui.label("Email address");
                    ui.add(egui::TextEdit::singleline(&mut d.email).desired_width(320.0).hint_text("you@example.com"));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !d.busy && signing_in.is_none() && d.email.contains('@'),
                                egui::Button::new(if d.busy { "Finding settings..." } else { "Find settings" }),
                            )
                            .clicked()
                        {
                            d.busy = true;
                            d.error = None;
                            d.found = None;
                            let _ = self.worker.send(Request::Discover(d.email.trim().to_string()));
                        }
                        if ui.button("Cancel").clicked() {
                            close_add = true;
                        }
                    });
                    if let Some(e) = &d.error {
                        ui.colored_label(egui::Color32::from_rgb(200, 60, 40), e);
                    }
                    if let Some(s) = &d.found {
                        ui.separator();
                        ui.label(format!("Found via {}{}", s.source, s.provider.as_deref().map(|p| format!(" ({p})")).unwrap_or_default()));
                        if let Some(i) = s.imap() {
                            ui.label(format!("Incoming: {}:{} ({:?})", i.host, i.port, i.security));
                        }
                        if let Some(o) = s.smtp() {
                            ui.label(format!("Outgoing: {}:{} ({:?})", o.host, o.port, o.security));
                        }
                        let Some(i) = s.imap() else { return };
                        let provider = Provider::for_host(&i.host);
                        let acct = Account {
                            email: d.email.trim().to_string(),
                            username: i.username.clone(),
                            imap: ServerCfg { host: i.host.clone(), port: i.port, security: i.security },
                            smtp: s.smtp().map(|o| ServerCfg { host: o.host.clone(), port: o.port, security: o.security }),
                            allow_insecure: false,
                            oauth: provider,
                            label: String::new(),
                            colour: None,
                            reply_to: String::new(),
                            organisation: String::new(),
                        };
                        ui.add_space(6.0);
                        match provider {
                            Some(p) => {
                                if signing_in.as_deref() == Some(acct.email.as_str()) {
                                    ui.label(RichText::new(format!("Finish signing in to {} in your browser...", p.name())).font(semibold(13.0)));
                                    // A provider that refuses shows its own page and never comes back.
                                    if ui.small_button("Start again").on_hover_text("The browser showed an error, or the tab was closed").clicked() {
                                        restart = true;
                                    }
                                } else if registration_and_button(ui, p, &mut d.client_id, &mut d.client_secret) {
                                    sign_in = Some(acct);
                                }
                            }
                            None => {
                                ui.label("Password (or app password)");
                                ui.add(egui::TextEdit::singleline(&mut d.password).password(true).desired_width(320.0));
                                if ui.add_enabled(!d.password.is_empty(), egui::Button::new("Add account")).clicked() {
                                    save = Some(acct);
                                }
                            }
                        }
                    }
                },
            );
            if sign_in.is_some() {
                d.error = None;
            }
            let password = d.password.clone();
            outcome = (save.map(|a| (a, password)), sign_in);
        }
        if let Some((acct, password)) = outcome.0 {
            let kept = secrets::set(&acct.email, &password);
            self.status = format!("Account added; password kept in {kept}.");
            self.add_account(acct);
            close_add = true;
        }
        if let Some(acct) = outcome.1 {
            self.start_sign_in(acct);
        }
        if close_add {
            self.add = None;
        }
        let mut done = None;
        let mut sign_in = None;
        if let Some((idx, pw)) = self.prompt.as_mut() {
            let acct = self.accounts.get(*idx).cloned();
            let email = acct.as_ref().map(|a| a.email.clone()).unwrap_or_default();
            let title = if acct.as_ref().is_some_and(|a| a.oauth.is_some()) { "Sign in needed" } else { "Password needed" };
            egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(
                ctx,
                |ui| match acct.as_ref().and_then(|a| a.oauth) {
                    Some(p) => {
                        ui.label(format!("{email} signs in with {}.", p.name()));
                        if let Some(e) = &self.prompt_error {
                            ui.colored_label(egui::Color32::from_rgb(200, 60, 40), e);
                        }
                        ui.add_space(4.0);
                        if signing_in.as_deref() == Some(email.as_str()) {
                            ui.label(RichText::new("Finish signing in in your browser...").font(semibold(13.0)));
                            if ui.small_button("Start again").on_hover_text("The browser showed an error, or the tab was closed").clicked() {
                                restart = true;
                            }
                        } else if registration_and_button(ui, p, &mut self.reg_id, &mut self.reg_secret) {
                            sign_in = acct.clone();
                        }
                        if ui.button("Cancel").clicked() {
                            done = Some((*idx, None));
                        }
                    }
                    None => {
                        ui.label(format!("Password for {email}"));
                        let r = ui.add(egui::TextEdit::singleline(pw).password(true).desired_width(300.0));
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() || enter {
                                done = Some((*idx, Some(pw.clone())));
                            }
                            if ui.button("Cancel").clicked() {
                                done = Some((*idx, None));
                            }
                        });
                    }
                },
            );
        }
        let mut move_to = None;
        let mut close_move = false;
        if self.moving.is_some() {
            // The folder they are all in (if they are all in one) is not a destination.
            let here = self.moving.as_ref().and_then(|(items, _)| {
                let first = items.first().map(|(f, _)| f.clone())?;
                items.iter().all(|(f, _)| *f == first).then_some(first)
            });
            let here = here.unwrap_or_default();
            let copying = self.moving.as_ref().is_some_and(|m| m.1);
            let rows = foldertree::rows(&self.folders, &Default::default());
            egui::Window::new(if copying { "Copy to folder" } else { "Move to folder" })
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        for r in &rows {
                            let Some(f) = r.folder.and_then(|i| self.folders.get(i)) else {
                                ui.label(format!("{}{}", "    ".repeat(r.depth), r.label));
                                continue;
                            };
                            let text = format!("{}{}", "    ".repeat(r.depth), r.label);
                            let item = egui::Button::new(text).frame_when_inactive(false).min_size(vec2(260.0, 22.0));
                            if ui.add_enabled(f.name != here, item).clicked() {
                                move_to = Some(f.name.clone());
                            }
                        }
                    });
                    ui.separator();
                    if ui.button("Cancel").clicked() {
                        close_move = true;
                    }
                });
        }
        if let Some(to) = move_to
            && let Some((items, copy)) = self.moving.take()
        {
            self.move_to(items, to, copy);
        }
        if close_move {
            self.moving = None;
        }
        if let Some(acct) = sign_in {
            self.prompt_error = None;
            self.start_sign_in(acct);
        }
        if let Some((idx, pw)) = done {
            self.prompt = None;
            self.prompt_error = None;
            if let (Some(pw), Some(a)) = (pw, self.accounts.get(idx)) {
                secrets::set(&a.email.clone(), &pw);
                self.sync(idx);
            }
        }
        if restart {
            self.signing_in = None;
            self.status = "Sign-in abandoned; you can try again.".into();
        }
    }

    /// Make an account the default: first in the list (and so opened at start).
    fn set_default_account(&mut self, i: usize) {
        if i == 0 || i >= self.accounts.len() {
            return;
        }
        let a = self.accounts.remove(i);
        self.accounts.insert(0, a);
        // The open account and the one being set up keep pointing at the same accounts.
        let moved = |x: usize| {
            if x == i {
                0
            } else if x < i {
                x + 1
            } else {
                x
            }
        };
        self.account = moved(self.account);
        self.settings_acct = moved(self.settings_acct);
        if let Err(e) = accounts::save(&self.accounts) {
            self.status = format!("Could not save the accounts: {e}");
        }
    }

    fn add_account(&mut self, acct: Account) {
        self.accounts.retain(|a| a.email != acct.email);
        self.accounts.push(acct);
        let _ = accounts::save(&self.accounts);
        self.account = self.accounts.len() - 1;
        self.folder = None;
        self.sync(self.account);
    }

    fn start_sign_in(&mut self, acct: Account) {
        self.signing_in = Some(acct.email.clone());
        self.status = "Waiting for the sign-in in your browser...".into();
        let _ = self.worker.send(Request::SignIn(acct));
    }

    /// The provider said the sign-in result: add the account, or carry on with the one that asked.
    fn signed_in(&mut self, email: String, r: Result<Option<String>, String>) {
        if self.signing_in.as_deref() == Some(email.as_str()) {
            self.signing_in = None;
        }
        // A send-only address signing in with its provider.
        if let Some(f) = self.sender_form.as_mut().filter(|f| f.busy && f.email.trim().eq_ignore_ascii_case(&email)) {
            f.busy = false;
            f.result = Some(r.map(|_| "Signed in. Save, then it can send.".to_string()));
            return;
        }
        let pending = self.add.as_ref().and_then(|d| {
            d.found.as_ref().and_then(|s| s.imap()).map(|i| (d.email.trim().to_string(), i.clone(), d.found.as_ref().and_then(|s| s.smtp()).cloned()))
        });
        match r {
            Ok(who) => {
                let note = match who {
                    Some(w) if !w.eq_ignore_ascii_case(&email) => format!("Signed in (the provider calls the account {w})."),
                    _ => "Signed in.".into(),
                };
                if let Some((e, i, o)) = pending.filter(|(e, _, _)| *e == email) {
                    self.add = None;
                    self.add_account(Account {
                        email: e,
                        username: i.username.clone(),
                        imap: ServerCfg { host: i.host.clone(), port: i.port, security: i.security },
                        smtp: o.map(|o| ServerCfg { host: o.host, port: o.port, security: o.security }),
                        allow_insecure: false,
                        oauth: Provider::for_host(&i.host),
                        label: String::new(),
                        colour: None,
                        reply_to: String::new(),
                        organisation: String::new(),
                    });
                } else if let Some(idx) = self.accounts.iter().position(|a| a.email == email) {
                    if self.prompt.as_ref().is_some_and(|(i, _)| *i == idx) {
                        self.prompt = None;
                    }
                    self.sync(idx);
                }
                self.status = note;
            }
            Err(e) => {
                if let Some(d) = self.add.as_mut().filter(|d| d.email.trim() == email) {
                    d.error = Some(e.clone());
                } else {
                    self.prompt_error = Some(e.clone());
                }
                self.status = format!("Not signed in: {e}");
            }
        }
    }

    /// An error that only a new sign-in can fix: ask for it.
    fn needs_sign_in(&mut self, email: &str, err: &str) -> bool {
        if !err.starts_with(worker::SIGN_IN_AGAIN) {
            return false;
        }
        if let Some(idx) = self.accounts.iter().position(|a| a.email == email) {
            self.prompt = Some((idx, String::new()));
            self.prompt_error = Some(err.trim_start_matches(worker::SIGN_IN_AGAIN).to_string());
        }
        true
    }
}

/// The app registration fields (only while MailCraft has none for this provider) and the
/// sign-in button. True when the button was pressed with a usable registration.
fn registration_and_button(ui: &mut egui::Ui, p: Provider, id: &mut String, secret: &mut String) -> bool {
    let have = accounts::oauth_client(p).is_some();
    if !have {
        ui.label(format!("First, MailCraft needs your own {} app registration (once only).", p.name()));
        ui.label(RichText::new("The steps are in MailCraft-sign-in-setup on the share.").font(regular(12.0)));
        ui.label("Client ID");
        ui.add(egui::TextEdit::singleline(id).desired_width(320.0));
        if p.needs_secret() {
            ui.label("Client secret");
            ui.add(egui::TextEdit::singleline(secret).password(true).desired_width(320.0));
        }
    }
    let client = mailcraft_core::oauth::Client {
        client_id: id.trim().to_string(),
        client_secret: Some(secret.trim().to_string()).filter(|s| !s.is_empty()),
        ..Default::default()
    };
    let ready = have || client.is_complete(p);
    if !have && !id.trim().is_empty() && !ready && p == Provider::Microsoft {
        ui.colored_label(
            egui::Color32::from_rgb(200, 60, 40),
            "That is not an Application (client) ID: it looks like 12345678-abcd-1234-abcd-1234567890ab.",
        );
    }
    ui.add_space(4.0);
    let pressed = ui.add_enabled(ready, egui::Button::new(RichText::new(format!("Sign in with {}", p.name())).font(semibold(13.0)))).clicked();
    if pressed && !have {
        // Keep the cleaned-up values, not whatever came along with the paste.
        let clean = mailcraft_core::oauth::Client { client_id: client.id(), client_secret: client.secret(), ..client };
        let _ = accounts::save_oauth_client(p, clean);
    }
    pressed && ready
}

/// The keyboard shortcuts, as Outlook has them where it has one.
/// What a single-key shortcut does.
#[derive(Clone, Copy)]
enum Does {
    Act(Action),
    Read,
    Unread,
    /// Next (1) or previous (-1) message.
    Nav(i32),
    /// Page Up/Down, Home, End.
    Jump(Jump),
}

/// A keyboard jump in the message list (31 Oct 2026: "page up/down and home and end
/// don't do anything in the email list").
#[derive(Clone, Copy)]
enum Jump {
    PageUp,
    PageDown,
    Home,
    End,
}

/// The jump a key asks for, if it is one.
fn jump_key(i: &mut egui::InputState) -> Option<Jump> {
    use egui::{Key, Modifiers as M};
    if i.consume_key(M::NONE, Key::PageDown) {
        Some(Jump::PageDown)
    } else if i.consume_key(M::NONE, Key::PageUp) {
        Some(Jump::PageUp)
    } else if i.consume_key(M::NONE, Key::Home) {
        Some(Jump::Home)
    } else if i.consume_key(M::NONE, Key::End) {
        Some(Jump::End)
    } else {
        None
    }
}

/// A key, or (for symbols, whose key differs between keyboards) the character it types.
#[derive(Clone, Copy)]
enum Press {
    K(egui::Key),
    C(&'static str),
}

const SHIFT: egui::Modifiers = egui::Modifiers::SHIFT;
const NONE: egui::Modifiers = egui::Modifiers::NONE;

/// The single-key sets (Settings > Accessibility), after each web mail's own; Shift forms
/// first, as a plain check would take them too. Each row: modifiers, key, what it does, how
/// the list of shortcuts names it.
type KeyRow = (egui::Modifiers, Press, Does, &'static str, &'static str);
const OUTLOOK_WEB_KEYS: &[KeyRow] = &[
    (SHIFT, Press::K(egui::Key::R), Does::Act(Action::ReplyAll), "Shift+R", "Reply all"),
    (SHIFT, Press::K(egui::Key::F), Does::Act(Action::Forward), "Shift+F", "Forward"),
    (NONE, Press::K(egui::Key::N), Does::Act(Action::NewMail), "N", "New email"),
    (NONE, Press::K(egui::Key::R), Does::Act(Action::Reply), "R", "Reply"),
    (NONE, Press::K(egui::Key::E), Does::Act(Action::Archive), "E", "Archive"),
    (NONE, Press::K(egui::Key::Delete), Does::Act(Action::Delete), "Delete", "Delete"),
    (NONE, Press::K(egui::Key::Q), Does::Read, "Q", "Mark as read"),
    (NONE, Press::K(egui::Key::U), Does::Unread, "U", "Mark as unread"),
    (NONE, Press::K(egui::Key::Insert), Does::Act(Action::ToggleFlag), "Insert", "Flag / unflag"),
    (NONE, Press::K(egui::Key::J), Does::Act(Action::Junk), "J", "Junk"),
    (NONE, Press::K(egui::Key::V), Does::Act(Action::MoveTo), "V", "Move to folder"),
    (NONE, Press::C("/"), Does::Act(Action::FocusSearch), "/", "Search"),
];
const GMAIL_KEYS: &[KeyRow] = &[
    (SHIFT, Press::K(egui::Key::I), Does::Read, "Shift+I", "Mark as read"),
    (SHIFT, Press::K(egui::Key::U), Does::Unread, "Shift+U", "Mark as unread"),
    (NONE, Press::K(egui::Key::C), Does::Act(Action::NewMail), "C", "New email"),
    (NONE, Press::K(egui::Key::R), Does::Act(Action::Reply), "R", "Reply"),
    (NONE, Press::K(egui::Key::A), Does::Act(Action::ReplyAll), "A", "Reply all"),
    (NONE, Press::K(egui::Key::F), Does::Act(Action::Forward), "F", "Forward"),
    (NONE, Press::K(egui::Key::E), Does::Act(Action::Archive), "E", "Archive"),
    (NONE, Press::C("#"), Does::Act(Action::Delete), "#", "Delete"),
    (NONE, Press::K(egui::Key::J), Does::Nav(1), "J / K", "Older / newer message"),
    (NONE, Press::K(egui::Key::K), Does::Nav(-1), "", ""),
    (NONE, Press::K(egui::Key::S), Does::Act(Action::ToggleFlag), "S", "Flag (star) / unflag"),
    (NONE, Press::C("!"), Does::Act(Action::Junk), "!", "Report spam (Junk)"),
    (NONE, Press::K(egui::Key::V), Does::Act(Action::MoveTo), "V", "Move to folder"),
    (NONE, Press::C("/"), Does::Act(Action::FocusSearch), "/", "Search"),
];
const YAHOO_KEYS: &[KeyRow] = &[
    (SHIFT, Press::K(egui::Key::K), Does::Unread, "Shift+K", "Mark as unread"),
    (NONE, Press::K(egui::Key::N), Does::Act(Action::NewMail), "N", "New email"),
    (NONE, Press::K(egui::Key::R), Does::Act(Action::Reply), "R", "Reply"),
    (NONE, Press::K(egui::Key::A), Does::Act(Action::ReplyAll), "A", "Reply all"),
    (NONE, Press::K(egui::Key::F), Does::Act(Action::Forward), "F", "Forward"),
    (NONE, Press::K(egui::Key::E), Does::Act(Action::Archive), "E", "Archive"),
    (NONE, Press::K(egui::Key::Delete), Does::Act(Action::Delete), "Delete", "Delete"),
    (NONE, Press::K(egui::Key::K), Does::Read, "K", "Mark as read"),
    (NONE, Press::K(egui::Key::L), Does::Act(Action::ToggleFlag), "L", "Flag / unflag"),
    (NONE, Press::K(egui::Key::D), Does::Act(Action::MoveTo), "D", "Move to folder"),
    (NONE, Press::C("/"), Does::Act(Action::FocusSearch), "/", "Search"),
];

/// The shortcuts in force, as (keys, what they do), for the list in Help and Settings.
fn shortcut_list(set: settings::KeySet) -> Vec<(String, String)> {
    let table = match set {
        settings::KeySet::Outlook => return SHORTCUTS.iter().map(|(k, w)| (k.to_string(), w.to_string())).collect(),
        settings::KeySet::Off => {
            return vec![
                ("Up / Down".into(), "Previous / next message".into()),
                ("Mouse back / forward buttons".into(), "Back / forward between folders".into()),
            ];
        }
        settings::KeySet::OutlookWeb => OUTLOOK_WEB_KEYS,
        settings::KeySet::Gmail => GMAIL_KEYS,
        settings::KeySet::Yahoo => YAHOO_KEYS,
    };
    let mut out: Vec<(String, String)> = table.iter().filter(|r| !r.3.is_empty()).map(|r| (r.3.to_string(), r.4.to_string())).collect();
    out.extend(
        [
            ("Up / Down", "Previous / next message"),
            ("F9", "Send / Receive"),
            ("F5", "Refresh the folder"),
            ("Ctrl+Z", "Undo the last delete"),
            ("Alt+Left / Alt+Right, mouse back / forward", "Back / forward between folders"),
            ("Ctrl+Enter", "Send (while writing)"),
            ("Ctrl+P", "Print"),
            ("Ctrl+W", "Close the tab"),
            ("Ctrl+Tab", "Next tab"),
            ("Ctrl+scroll", "Zoom the message"),
        ]
        .iter()
        .map(|(k, w)| (k.to_string(), w.to_string())),
    );
    out
}

const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+N", "New email"),
    ("Ctrl+R", "Reply"),
    ("Ctrl+Shift+R", "Reply all"),
    ("Ctrl+F", "Forward"),
    ("Ctrl+Enter", "Send (while writing)"),
    ("Delete", "Delete"),
    ("Ctrl+Z", "Undo the last delete"),
    ("Ctrl+Shift+V", "Move to folder"),
    ("Ctrl+Q / Ctrl+U", "Mark as read / unread"),
    ("Insert", "Flag / unflag"),
    ("Up / Down", "Previous / next message"),
    ("Ctrl+click / Shift+click", "Select several messages / a range"),
    ("Shift+Up / Down, Ctrl+A", "Extend the selection / select all"),
    ("Ctrl+E", "Search"),
    ("F9", "Send / Receive"),
    ("F5", "Refresh the folder"),
    ("Alt+Left / Alt+Right, mouse back / forward", "Back / forward between folders"),
    ("Ctrl+P", "Print"),
    ("Ctrl+Shift+E", "New folder"),
    ("Ctrl+F1", "Collapse / show the ribbon"),
    ("Ctrl+W / Ctrl+F4", "Close the tab (a message, Settings, Account settings) or a message's own window"),
    ("Ctrl+Tab / Ctrl+Shift+Tab", "Next / previous tab"),
    ("Ctrl+scroll, Ctrl+plus / minus / 0", "Zoom the message in / out / back to normal"),
];

/// Right-click menu for a message (None = a separator).
const MESSAGE_MENU: &[Option<(Action, &str)>] = &[
    Some((Action::Reply, "Reply")),
    Some((Action::ReplyAll, "Reply all")),
    Some((Action::Forward, "Forward")),
    Some((Action::ForwardAttachment, "Forward as attachment")),
    None,
    Some((Action::ToggleRead, "Mark as read / unread")),
    Some((Action::ToggleFlag, "Flag / unflag")),
    Some((Action::Categorise, "Categorise...")),
    None,
    Some((Action::MoveTo, "Move to folder...")),
    Some((Action::CopyTo, "Copy to folder...")),
    Some((Action::CreateRule, "Create rule...")),
    Some((Action::Archive, "Archive")),
    Some((Action::Junk, "Junk")),
    Some((Action::NotJunk, "Not junk")),
    Some((Action::BlockSender, "Block sender")),
    Some((Action::AlwaysOther, "Always move to Other")),
    Some((Action::AlwaysFocused, "Always move to Focused")),
    Some((Action::Sweep, "Sweep...")),
    Some((Action::Restore, "Restore")),
    Some((Action::Delete, "Delete")),
    None,
    Some((Action::SaveAs, "Save as...")),
    Some((Action::ViewSource, "View source")),
    Some((Action::Print, "Print")),
];

/// Right-click menu for a folder.
const FOLDER_MENU: &[Option<(Action, &str)>] = &[
    Some((Action::NewSubfolder, "New subfolder...")),
    Some((Action::RenameFolder, "Rename...")),
    Some((Action::DeleteFolder, "Delete folder...")),
    None,
    Some((Action::MarkAllRead, "Mark all as read")),
    Some((Action::EmptyFolder, "Empty folder...")),
    None,
    Some((Action::SendReceive, "Update folders")),
    Some((Action::RefreshFolder, "Refresh this folder")),
];

/// Folder colours offered (Betterbird-style), with their names.
const FOLDER_COLOURS: &[(&str, [u8; 3])] = &[
    ("Red", [0xE7, 0x48, 0x56]),
    ("Orange", [0xF7, 0x63, 0x0C]),
    ("Yellow", [0xE8, 0xB3, 0x0A]),
    ("Green", [0x16, 0xA3, 0x4A]),
    ("Teal", [0x03, 0x83, 0x87]),
    ("Blue", [0x00, 0x78, 0xD4]),
    ("Purple", [0x88, 0x6C, 0xE4]),
    ("Pink", [0xE3, 0x00, 0x8C]),
    ("Brown", [0x8E, 0x56, 0x2E]),
    ("Grey", [0x69, 0x79, 0x7E]),
];

/// Type text into the editor where the cursor is (new lines start new paragraphs).
fn insert_text(editor: &mut WordApp, text: &str) {
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            let _ = editor.run("text.newParagraph", serde_json::json!({}));
        }
        let line = line.trim_end_matches('\r');
        if !line.is_empty() {
            let _ = editor.run("text.insert", serde_json::json!({ "text": line }));
        }
    }
}

/// One plain menu item, highlighted under the mouse.
fn menu_item(ui: &mut egui::Ui, label: &str) -> bool {
    ui.add(egui::Button::new(label).frame_when_inactive(false).min_size(vec2(210.0, 22.0))).clicked()
}

/// The colour list for folders: Some(colour), Some(None) for "no colour", None if nothing chosen.
fn colour_menu(ui: &mut egui::Ui) -> Option<Option<[u8; 3]>> {
    let mut out = None;
    for (name, c) in FOLDER_COLOURS {
        let resp = ui.add(egui::Button::new(format!("      {name}")).frame_when_inactive(false).min_size(vec2(150.0, 22.0)));
        let sw = egui::Rect::from_center_size(resp.rect.left_center() + vec2(14.0, 0.0), vec2(12.0, 12.0));
        ui.painter().rect_filled(sw, 2.0, egui::Color32::from_rgb(c[0], c[1], c[2]));
        if resp.clicked() {
            out = Some(Some(*c));
        }
    }
    ui.separator();
    if ui.add(egui::Button::new("No colour").frame_when_inactive(false).min_size(vec2(150.0, 22.0))).clicked() {
        out = Some(None);
    }
    out
}

/// Draw a context menu from a table; returns the item chosen.
fn menu(ui: &mut egui::Ui, items: &[Option<(Action, &str)>], state: &dyn Fn(Action) -> ribbon::State) -> Option<Action> {
    let mut out = None;
    ui.set_min_width(210.0);
    for item in items {
        match item {
            None => {
                ui.separator();
            }
            Some((a, label)) => {
                let item = egui::Button::new(*label).frame_when_inactive(false).min_size(vec2(210.0, 22.0));
                if ui.add_enabled(state(*a).enabled, item).clicked() {
                    out = Some(*a);
                    ui.close();
                }
            }
        }
    }
    out
}

/// A small red flag at `at` (its top-left), for flagged messages in the list.
fn draw_flag(p: &egui::Painter, at: egui::Pos2) {
    let red = egui::Color32::from_rgb(0xD1, 0x3B, 0x3B);
    p.line_segment([at + vec2(1.0, 0.0), at + vec2(1.0, 13.0)], egui::Stroke::new(1.3, red));
    p.add(egui::Shape::convex_polygon(
        vec![at + vec2(1.0, 0.5), at + vec2(10.0, 0.5), at + vec2(8.0, 3.5), at + vec2(10.0, 6.5), at + vec2(1.0, 6.5)],
        red,
        egui::Stroke::NONE,
    ));
}

/// A folder dragged from the tree towards Favourites: the folder and its name (for the label).
struct DragFolder(accounts::FolderRef, String);

/// A favourite being dragged to a new place: the folder and its name (for the label).
struct DragFav(accounts::FolderRef, String);

/// What is carried while messages are dragged to a folder.
struct DragMail(Vec<(String, u32)>);

/// The label that follows the pointer while messages are dragged.
fn drag_label(ctx: &egui::Context, t: &Tokens) {
    let Some(at) = ctx.pointer_latest_pos() else { return };
    let text = if let Some(p) = egui::DragAndDrop::payload::<DragMail>(ctx) {
        let n = p.0.len();
        let what = if n == 1 { "1 message".to_string() } else { format!("{n} messages") };
        if ctx.input(|i| i.modifiers.command) { format!("Copy {what}") } else { format!("Move {what}") }
    } else if let Some(f) = egui::DragAndDrop::payload::<DragFav>(ctx) {
        f.1.clone()
    } else if let Some(f) = egui::DragAndDrop::payload::<DragFolder>(ctx) {
        format!("Add {} to Favourites", f.1)
    } else {
        return;
    };
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("mc_drag")));
    let galley = painter.layout_no_wrap(text, semibold(12.5), egui::Color32::WHITE);
    let rect = egui::Rect::from_min_size(at + vec2(14.0, 10.0), galley.size() + vec2(16.0, 10.0));
    painter.rect_filled(rect, 4.0, t.accent);
    painter.galley(rect.min + vec2(8.0, 5.0), galley, egui::Color32::WHITE);
}

/// Messages grouped by folder (in the order the folders first appear): IMAP works per folder.
fn by_folder(items: &[(String, u32)]) -> Vec<(String, Vec<u32>)> {
    let mut out: Vec<(String, Vec<u32>)> = vec![];
    for (f, u) in items {
        match out.iter_mut().find(|(g, _)| g == f) {
            Some((_, v)) => v.push(*u),
            None => out.push((f.clone(), vec![*u])),
        }
    }
    out
}

fn flagged(m: &MessageRow) -> bool {
    m.flags.split_whitespace().any(|f| f.eq_ignore_ascii_case("\\Flagged"))
}

/// A subject without its Re:/Fwd: prefixes, lower case, for sorting.
fn bare_subject(s: &str) -> String {
    let mut t = s.trim();
    loop {
        let lower = t.to_ascii_lowercase();
        let Some(p) = ["re:", "fw:", "fwd:", "aw:", "sv:"].iter().find(|p| lower.starts_with(**p)) else { break };
        t = t.get(p.len()..).unwrap_or("").trim_start();
    }
    t.to_lowercase()
}

/// A subject made safe to use as a file name.
fn safe_file_name(s: &str) -> String {
    let cleaned: String =
        s.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_end_matches('.').chars().take(80).collect::<String>();
    if cleaned.is_empty() { "message".into() } else { cleaned }
}

/// Search results newest first, each message once: Gmail also files every message in
/// "All Mail", so a copy there is dropped when the same message is in a real folder.
fn tidy_results(mut found: Vec<MessageRow>, folders: &[FolderRow]) -> Vec<MessageRow> {
    let is_all = |f: &str| folders.iter().any(|x| x.name == f && x.special.as_deref() == Some("\\All"));
    found.sort_by_key(|m| is_all(&m.folder));
    let mut seen_ids = std::collections::HashSet::new();
    let mut seen_rows = std::collections::HashSet::new();
    found.retain(|m| seen_rows.insert((m.folder.clone(), m.uid)) && m.message_id.as_ref().is_none_or(|id| seen_ids.insert(id.clone())));
    found.sort_by(|a, b| b.date.cmp(&a.date).then(b.uid.cmp(&a.uid)));
    found
}

/// Where each message in Trash goes back to, one move per folder: where it was deleted from
/// when that is known and the folder is still there (`exists`), else the Inbox.
/// A delete into Trash that Ctrl+Z can take back.
struct Undo {
    account: String,
    /// The Message-IDs of what was deleted (UIDs change on the way into Trash).
    ids: Vec<String>,
    /// How many had no Message-ID: those can only be restored from Trash by hand.
    without_id: usize,
}

/// How many deletes Ctrl+Z goes back through, and how long an undo waits for its delete.
const UNDO_KEEP: usize = 20;
const UNDO_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Of an undo, the Trash UIDs found for it (`found`: Message-ID, UID) and what is still to come.
fn split_undo(u: Undo, found: &[(String, u32)]) -> (Vec<u32>, Option<Undo>) {
    let here: Vec<u32> = found.iter().map(|(_, uid)| *uid).collect();
    let ids: Vec<String> = u.ids.into_iter().filter(|id| !found.iter().any(|(f, _)| f == id)).collect();
    let rest = (!ids.is_empty()).then_some(Undo { account: u.account, ids, without_id: 0 });
    (here, rest)
}

fn plan_restore(
    trash: &str,
    uids: &[u32],
    origins: &[mailcraft_core::store::Origin],
    exists: &dyn Fn(&str) -> bool,
    inbox: &str,
    label: &dyn Fn(&str) -> String,
) -> Vec<worker::Restore> {
    let mut plan: Vec<worker::Restore> = vec![];
    for u in uids {
        let o = origins.iter().find(|o| o.uid == *u);
        let known = o.and_then(|o| o.folder.clone()).filter(|f| f != trash && exists(f));
        let unknown = usize::from(known.is_none());
        let to = known.unwrap_or_else(|| inbox.to_string());
        let ids: Vec<String> = o.and_then(|o| o.message_id.clone()).into_iter().collect();
        match plan.iter_mut().find(|r| r.to == to) {
            Some(r) => {
                r.uids.push(*u);
                r.message_ids.extend(ids);
                r.unknown += unknown;
            }
            None => plan.push(worker::Restore { trash: trash.to_string(), uids: vec![*u], label: label(&to), to, message_ids: ids, unknown }),
        }
    }
    plan
}

/// A folder's name as the folder pane shows it.
fn folder_label(name: &str, folders: &[FolderRow]) -> String {
    match folders.iter().find(|f| f.name == name) {
        Some(f) if f.special.as_deref() == Some("\\Inbox") || name.eq_ignore_ascii_case("INBOX") => "Inbox".into(),
        Some(f) => f.delimiter.as_deref().filter(|d| !d.is_empty()).and_then(|d| name.rsplit(d).next()).unwrap_or(name).to_string(),
        None => name.to_string(),
    }
}

struct App(MailApp);

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let app = &mut self.0;
        if app.styled == 0 {
            wordcraft_ui_egui::theme::install_fonts(ctx);
        }
        app.apply_theme(ctx);
        app.apply_view(ctx);
        app.styled = app.styled.saturating_add(1);
        htmlview::pump();
        app.drain();
        app.notify_while_hidden(ctx);
        let t = app.look().tokens();
        app.popouts_ui(ctx, &t);
        if app.searching && app.search.trim().is_empty() {
            app.stop_search();
        }
        app.take_links(ctx);
        app.timers(ctx);
        // Keys pressed in the message (Ctrl+W, Delete, Ctrl+R...) are MailCraft's: in before
        // anything looks for them.
        #[cfg(any(windows, target_os = "linux"))]
        if let Some(w) = app.web.as_ref() {
            let keys = w.take_keys();
            if !keys.is_empty() {
                ctx.input_mut(|i| i.events.extend(keys));
            }
        }
        app.tab_keys(ctx);
        if app.dialog.is_none()
            && app.add.is_none()
            && app.prompt.is_none()
            && let Some(a) = app.keytip_keys(ctx)
        {
            app.act(a, ctx);
        }
        let typing = ctx.egui_wants_keyboard_input();
        if !typing
            && app.compose.is_none()
            && app.add.is_none()
            && app.prompt.is_none()
            && app.moving.is_none()
            && app.dialog.is_none()
            && let Some(a) = app.shortcut(ctx)
        {
            app.act(a, ctx);
        }
        app.note_visit();
        // An undo waiting for its delete looks again each second (its delete may have failed
        // and been synced back, or it may have run out of time), not only when a move ends.
        if let Some((_, _, checked)) = &app.undo_wait {
            if checked.elapsed() >= std::time::Duration::from_secs(1)
                && let Some((u, since, _)) = app.undo_wait.take()
            {
                app.finish_undo(u, since);
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        if app.compose.is_none()
            && app.add.is_none()
            && app.prompt.is_none()
            && app.moving.is_none()
            && app.dialog.is_none()
            && let Some(a) = app.folder_keys(ctx, typing)
        {
            app.act(a, ctx);
        }
        if let Some(open) = app.open.as_mut() {
            open.reader.logic(ctx);
        }
        if let Some(c) = app.compose.as_mut() {
            // Ctrl+Enter sends, as in Outlook (before the editor takes the Enter as a new paragraph).
            if !c.sending && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)) {
                c.send_now = true;
            }
            // Files dropped on the window while writing are attachments (WordCraft would insert them).
            for f in ctx.input_mut(|i| std::mem::take(&mut i.raw.dropped_files)) {
                let path: std::path::PathBuf = f.path().to_path_buf();
                let read = read_attachment(&path);
                match read {
                    Ok(a) => c.attachments.push(a),
                    Err(e) => c.error = Some(format!("Could not attach {e}")),
                }
            }
            c.editor.logic(ctx);
            // Autosave: every so often, save if anything changed since the last save.
            if !c.sending && c.saving.is_none() && c.checked.elapsed() >= AUTOSAVE {
                c.checked = std::time::Instant::now();
                if c.fingerprint() != c.saved {
                    app.save_draft();
                }
            }
            ctx.request_repaint_after(AUTOSAVE);
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let app = &mut self.0;
        if app.styled < 2 {
            ui.ctx().request_repaint();
            return;
        }
        let t = app.look().tokens();
        app.toolbar(ui, &t);
        app.tab_bar(ui, &t);
        app.spaces_bar(ui, &t);
        if app.calendar_active() {
            app.hide_web();
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.ribbon)).show(ui, |ui| app.calendar_view(ui, &t));
        } else if let Some(MainTab::Message { folder, uid, .. }) = app.tabs.get(app.active_tab).cloned() {
            // One message fills the window. Coming back to its tab opens it again if the list
            // has moved on (once: a message that will not open is not asked for again and again).
            let showing = app.open.as_ref().is_some_and(|o| o.folder == folder && o.uid == uid);
            if !showing && app.selected.as_ref() != Some(&(folder.clone(), uid)) {
                app.open_message(folder, uid);
            }
            app.reading_pane(ui, &t, frame);
        } else if !matches!(app.tabs.get(app.active_tab), Some(MainTab::Mail) | None) {
            // A settings tab fills the window; the mail (and its browser view) waits behind it.
            app.hide_web();
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.ribbon)).show(ui, |ui| app.settings_view(ui, &t));
        } else {
            if app.show_folders {
                app.folder_pane(ui, &t);
            }
            app.mail_panes(ui, &t, frame);
        }
        let ctx = ui.ctx().clone();
        app.dialogs(&ctx);
        app.transfer_dialog(&ctx, &t);
        app.draw_banners(&ctx, &t);
        app.draw_card(&ctx, &t);
        drag_label(&ctx, &t);
        app.after_frame(&ctx);
    }
}

impl MailApp {
    fn mail_panes(&mut self, ui: &mut egui::Ui, t: &Tokens, frame: &mut eframe::Frame) {
        let app = self;
        match (app.pane_at, app.compose.is_some()) {
            // Reading pane off: the list fills the window; writing takes it over.
            (PaneAt::Off, false) => {
                app.hide_web();
                app.list_pane(ui, t);
            }
            (PaneAt::Off, true) => app.reading_pane(ui, t, frame),
            _ => {
                app.list_pane(ui, t);
                app.reading_pane(ui, t, frame);
            }
        }
    }

    /// The window's title, the taskbar flash and where the window is, after each frame.
    fn after_frame(&mut self, ctx: &egui::Context) {
        let app = self;
        app.drag_copy = egui::DragAndDrop::has_any_payload(ctx) && ctx.input(|i| i.modifiers.command);
        let title = match (&app.open, app.current()) {
            _ if app.compose.is_some() => format!(
                "{} - MailCraft",
                app.compose.as_ref().map(|c| if c.subject.is_empty() { c.kind.to_string() } else { c.subject.clone() }).unwrap_or_default()
            ),
            (Some(o), _) => format!("{} - MailCraft", o.subject),
            (None, Some(a)) => format!("{} - MailCraft", a.email),
            _ => "MailCraft".into(),
        };
        // Only when it changes: each title sent makes Windows redraw the taskbar button, and
        // sending it every frame kept Explorer (and so the taskbar) busy.
        if app.last_title != title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            app.last_title = title;
        }
        // MAILCRAFT_DEBUG_REPAINT=1: log why frames are drawn (to find what keeps it busy).
        if std::env::var_os("MAILCRAFT_DEBUG_REPAINT").is_some() {
            let causes = ctx.repaint_causes();
            if !causes.is_empty() {
                eprintln!("repaint: {causes:?}");
            }
        }
        // Where the window is, to open there next time (not while minimised; when maximised,
        // keep the size it had before so un-maximising goes back to it).
        let (outer, inner, maximized, minimized, closing) = ctx.input(|i| {
            let v = i.viewport();
            (v.outer_rect, v.inner_rect, v.maximized.unwrap_or(false), v.minimized.unwrap_or(false), v.close_requested())
        });
        if !minimized {
            app.maximized = maximized;
            if !maximized && let (Some(o), Some(i)) = (outer, inner) {
                app.place = Some(Place { x: o.min.x.round(), y: o.min.y.round(), w: i.width().round(), h: i.height().round() });
            }
        }
        app.save_view(ctx, closing);
        if closing {
            mailto::forget();
        }
    }
}

fn main() -> eframe::Result {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("MailCraft {}", option_env!("MAILCRAFT_VERSION").filter(|v| !v.is_empty()).unwrap_or(env!("CARGO_PKG_VERSION")));
        return Ok(());
    }
    // One window: an email link clicked elsewhere, or MailCraft started again from its icon,
    // goes to the MailCraft already open if there is one (--new-window: start another anyway).
    let first = std::env::args().nth(1);
    // A sample of the notification MailCraft shows while minimised, to check Windows shows it.
    if first.as_deref() == Some("--test-notification") {
        let card = Card {
            heading: "New mail \u{00b7} test".into(),
            title: "MailCraft".into(),
            lines: vec!["A test notification".into(), "If you can read this, new-mail notifications work.".into()],
            open: None,
            day: None,
            born: std::time::Instant::now(),
        };
        let shown = notify::show(&card, std::sync::mpsc::channel().0, egui::Context::default());
        std::process::exit(if shown { 0 } else { 1 });
    }
    let link = first.clone().filter(|a| mailto::is_link(a));
    let single = first.as_deref() != Some("--new-window");
    if single && mailto::hand_to_running(link.as_deref().unwrap_or(mailto::SHOW)) {
        return Ok(());
    }
    logfile::init();
    log::info!(
        "MailCraft {} started{}",
        option_env!("MAILCRAFT_VERSION").filter(|v| !v.is_empty()).unwrap_or(env!("CARGO_PKG_VERSION")),
        if single { "" } else { " (another window)" }
    );
    // Open where it was closed.
    let v: View = accounts::read_settings("view.json").unwrap_or_default();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("MailCraft")
        .with_app_id("uk.perkin.mailcraft")
        .with_inner_size([1400.0, 880.0])
        .with_min_inner_size([900.0, 520.0])
        .with_maximized(v.maximized);
    if let Some(p) = v.window.filter(Place::usable) {
        viewport = viewport.with_inner_size([p.w, p.h]).with_position([p.x, p.y]);
    }
    // The window's (and taskbar's, and Alt-Tab's) icon: the postage stamp.
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/app-icon/mailcraft-256.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("MailCraft", options, Box::new(move |cc| Ok(Box::new(App(MailApp::new(&cc.egui_ctx, link, single))))))
}

#[cfg(test)]
mod tests {
    #[test]
    fn undo_restores_what_reached_trash_and_waits_for_the_rest() {
        let u = Undo { account: "a".into(), ids: vec!["<1>".into(), "<2>".into(), "<3>".into()], without_id: 1 };
        let (here, rest) = split_undo(u, &[("<1>".into(), 40), ("<3>".into(), 42)]);
        assert_eq!(here, vec![40, 42]);
        let rest = rest.unwrap();
        assert_eq!((rest.account.as_str(), rest.ids.clone()), ("a", vec!["<2>".to_string()]));
        // All there: nothing left to wait for.
        let (here, rest) = split_undo(rest, &[("<2>".into(), 43)]);
        assert_eq!(here, vec![43]);
        assert!(rest.is_none());
        // None there yet (the delete still on its way): all of it waits.
        let u = Undo { account: "a".into(), ids: vec!["<9>".into()], without_id: 0 };
        let (here, rest) = split_undo(u, &[]);
        assert!(here.is_empty() && rest.is_some_and(|r| r.ids == vec!["<9>".to_string()]));
    }

    #[test]
    fn restore_goes_back_where_it_came_from_else_the_inbox() {
        use mailcraft_core::store::Origin;
        let o = |uid, id: Option<&str>, folder: Option<&str>| Origin { uid, message_id: id.map(str::to_string), folder: folder.map(str::to_string) };
        let origins = [
            o(1, Some("<a@x>"), Some("Bills")),
            o(2, Some("<b@x>"), Some("Projects/Old")),
            o(3, Some("<c@x>"), None),
            o(4, None, None),
            o(5, Some("<e@x>"), Some("Bills")),
            o(6, Some("<f@x>"), Some("Trash")),
            o(7, Some("<g@x>"), Some("INBOX")),
        ];
        let exists = |f: &str| ["INBOX", "Bills", "Trash"].contains(&f);
        let label = |f: &str| if f == "INBOX" { "Inbox".to_string() } else { f.to_string() };
        // 8 is not in this PC's copy of Trash at all: the Inbox, not known.
        let plan = super::plan_restore("Trash", &[1, 2, 3, 4, 5, 6, 7, 8], &origins, &exists, "INBOX", &label);
        assert_eq!(plan.len(), 2, "one move per folder");
        let bills = plan.iter().find(|r| r.to == "Bills").unwrap();
        assert_eq!((bills.uids.clone(), bills.unknown, bills.trash.as_str()), (vec![1, 5], 0, "Trash"));
        assert_eq!(bills.message_ids, vec!["<a@x>".to_string(), "<e@x>".to_string()]);
        let inbox = plan.iter().find(|r| r.to == "INBOX").unwrap();
        // 2: its folder has gone; 3, 4, 8: not known; 6: "from Trash" is no place to go back to;
        // 7: really from the Inbox.
        assert_eq!(inbox.uids, vec![2, 3, 4, 6, 7, 8]);
        assert_eq!((inbox.unknown, inbox.label.as_str()), (5, "Inbox"));
        assert_eq!(inbox.message_ids.len(), 4, "every Message-ID, so stale records go too");
        assert!(super::plan_restore("Trash", &[], &origins, &exists, "INBOX", &label).is_empty());
    }

    use super::*;

    #[test]
    fn date_headings() {
        // Friday 9 October 2026.
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let at = |d: &str| {
            let local = chrono::NaiveDateTime::parse_from_str(&format!("{d} 12:00"), "%Y-%m-%d %H:%M").unwrap();
            let t = local.and_local_timezone(chrono::Local).unwrap().with_timezone(&chrono::Utc);
            day_label(&Some(t.to_rfc3339()), today)
        };
        assert_eq!(at("2026-10-09"), "Today");
        assert_eq!(at("2026-10-08"), "Yesterday");
        assert_eq!(at("2026-10-06"), "Tuesday");
        assert_eq!(at("2026-10-01"), "Last week");
        assert_eq!(at("2026-09-30"), "Last week");
        assert_eq!(at("2026-09-27"), "September 2026");
        assert_eq!(day_label(&None, today), "Older");
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
        assert_eq!(at_with(today, "2026-10-03"), "Earlier this month");

        fn at_with(today: chrono::NaiveDate, d: &str) -> String {
            let local = chrono::NaiveDateTime::parse_from_str(&format!("{d} 12:00"), "%Y-%m-%d %H:%M").unwrap();
            let t = local.and_local_timezone(chrono::Local).unwrap().with_timezone(&chrono::Utc);
            day_label(&Some(t.to_rfc3339()), today)
        }
    }

    #[test]
    fn sender_initials() {
        assert_eq!(initials("Sam Taylor"), "ST");
        assert_eq!(initials("news@shop.example"), "N");
        assert_eq!(initials("Royal Mail"), "RM");
        assert_eq!(initials(""), "?");
        assert_eq!(without_quote("Thanks!\n\nOn Thu, Mark wrote:\n> old"), "Thanks!");
        assert_eq!(without_quote("Yes\n> quoted\nNo"), "Yes\nNo");
    }
}
