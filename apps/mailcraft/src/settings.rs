//! Settings (settings.json): what the Settings window sets, after Outlook's own.
//! Layout choices made on the ribbon (theme, reading pane, conversations...) stay in view.json.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Density {
    #[default]
    Roomy,
    Cozy,
    Compact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MarkRead {
    /// As soon as it is opened.
    #[default]
    OnOpen,
    /// After it has been open a few seconds.
    AfterSeconds,
    Never,
}

/// How much mail is kept on this PC (Settings > Storage), for reading offline and for search
/// that finds words in the messages without asking the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Offline {
    /// Every message, downloaded in the background after each sync.
    #[default]
    All,
    /// Mail from the last 12 months.
    Year,
    /// Only messages that have been opened.
    Opened,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Pictures {
    /// Blocked until "Show pictures".
    #[default]
    Ask,
    /// Always loaded (senders can tell when the message was read).
    Always,
    /// Loaded for safe senders only.
    SafeSenders,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AfterDelete {
    #[default]
    OpenNext,
    OpenPrevious,
    ReturnToList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Receipts {
    #[default]
    Ask,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DateStyle {
    /// 09/10/2026 (UK).
    #[default]
    DayMonth,
    /// 10/09/2026 (US).
    MonthDay,
    /// 2026-10-09.
    Iso,
}

/// Text size (Settings > Message list): the whole window scales, as Outlook's does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TextSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl TextSize {
    pub fn zoom(self) -> f32 {
        match self {
            TextSize::Small => 0.9,
            TextSize::Medium => 1.0,
            TextSize::Large => 1.15,
        }
    }
}

/// How many rows each message takes in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Rows {
    Multiple,
    One,
    /// One row when the list is wide (the reading pane at the bottom or off, or a wide list),
    /// as Outlook does unless told otherwise.
    #[default]
    BySize,
}

/// Columns the one-row list can show (Betterbird's); Subject is always there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ListCol {
    /// A tick box per message (and select all in the heading).
    Select,
    /// Conversations: open or close one.
    Thread,
    Flag,
    Attachment,
    /// Read or unread, as a dot; click to change.
    Read,
    From,
    /// A delete button.
    Delete,
    /// Always shown; it takes the room the others leave.
    Subject,
    Recipient,
    /// Who it is from, or (for what was sent) who it went to.
    Correspondents,
    /// Junk or not; click to change.
    Junk,
    /// When it was sent.
    Date,
    /// When it arrived.
    Received,
    /// New, read, replied, forwarded.
    Status,
    Size,
    /// Categories.
    Tag,
    Account,
    Priority,
    /// Unread messages in a conversation.
    Unread,
    /// Messages in a conversation.
    Total,
    /// The folder.
    Location,
    /// The order messages arrived in the folder.
    Order,
}

impl ListCol {
    /// Betterbird's order (the order a column goes back to when it is turned on).
    pub const ALL: [ListCol; 22] = [
        ListCol::Select,
        ListCol::Thread,
        ListCol::Flag,
        ListCol::Attachment,
        ListCol::Read,
        ListCol::From,
        ListCol::Delete,
        ListCol::Subject,
        ListCol::Recipient,
        ListCol::Correspondents,
        ListCol::Junk,
        ListCol::Date,
        ListCol::Received,
        ListCol::Status,
        ListCol::Size,
        ListCol::Tag,
        ListCol::Account,
        ListCol::Priority,
        ListCol::Unread,
        ListCol::Total,
        ListCol::Location,
        ListCol::Order,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ListCol::Select => "Select messages",
            ListCol::Thread => "Thread",
            ListCol::Flag => "Flag",
            ListCol::Attachment => "Attachments",
            ListCol::Read => "Read status",
            ListCol::From => "From",
            ListCol::Delete => "Delete",
            ListCol::Subject => "Subject",
            ListCol::Recipient => "Recipient",
            ListCol::Correspondents => "Correspondents",
            ListCol::Junk => "Junk",
            ListCol::Date => "Date",
            ListCol::Received => "Received",
            ListCol::Status => "Status",
            ListCol::Size => "Size",
            ListCol::Tag => "Tag",
            ListCol::Account => "Account",
            ListCol::Priority => "Priority",
            ListCol::Unread => "Unread",
            ListCol::Total => "Total",
            ListCol::Location => "Location",
            ListCol::Order => "Order received",
        }
    }

    /// Its heading (short enough for the column).
    pub fn heading(self) -> &'static str {
        match self {
            ListCol::Order => "Order",
            ListCol::Priority => "Priority",
            c => c.name(),
        }
    }

    /// Turn a column on where Betterbird's order puts it among those already shown.
    pub fn insert_in_order(list: &mut Vec<ListCol>, c: ListCol) {
        if list.contains(&c) {
            return;
        }
        let rank = |x: &ListCol| ListCol::ALL.iter().position(|y| y == x).unwrap_or(0);
        let at = list.iter().rposition(|x| rank(x) < rank(&c)).map(|i| i + 1).unwrap_or(0);
        list.insert(at, c);
    }

    /// Move a column to `to` (a place among the others, before it is taken out).
    pub fn move_to(list: &mut Vec<ListCol>, c: ListCol, to: usize) {
        let Some(from) = list.iter().position(|x| *x == c) else { return };
        let item = list.remove(from);
        let to = if to > from { to - 1 } else { to };
        list.insert(to.min(list.len()), item);
    }

    /// The narrowest it can be dragged to.
    pub fn min_width(self) -> f32 {
        if self.is_icon() { 18.0 } else { 40.0 }
    }

    /// A narrow column with an icon for a heading.
    pub fn is_icon(self) -> bool {
        matches!(self, ListCol::Select | ListCol::Thread | ListCol::Flag | ListCol::Attachment | ListCol::Read | ListCol::Delete | ListCol::Junk)
    }

    /// Its width until it is dragged (every column's edge can be; the widths are kept in view.json).
    pub fn width(self, from: f32) -> f32 {
        match self {
            c if c.is_icon() => 24.0,
            ListCol::From => from,
            ListCol::Recipient => 180.0,
            ListCol::Correspondents => 200.0,
            ListCol::Date | ListCol::Received => 104.0,
            ListCol::Status => 78.0,
            ListCol::Size => 64.0,
            ListCol::Tag => 130.0,
            ListCol::Account => 170.0,
            ListCol::Priority => 66.0,
            ListCol::Unread => 58.0,
            ListCol::Total => 48.0,
            ListCol::Location => 120.0,
            _ => 64.0,
        }
    }
}

/// Preview text under (or beside) the subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Preview {
    Off,
    /// After the subject, on its line.
    #[default]
    Inline,
    OneLine,
    TwoLines,
}

/// Which keyboard shortcuts (Settings > Accessibility), as Outlook on the web offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum KeySet {
    /// Outlook for Windows: Ctrl+R, Ctrl+N...
    #[default]
    Outlook,
    /// Outlook on the web: single letters (N, R, E, Q...).
    OutlookWeb,
    Gmail,
    Yahoo,
    Off,
}

/// Where searches look (Settings > Search), as Outlook offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SearchScope {
    /// All folders when searching from the Inbox; the open folder otherwise.
    #[default]
    FromInbox,
    All,
    Current,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Animations {
    #[default]
    System,
    On,
    Off,
}

/// Automatic replies, sent by MailCraft itself while it is running (IMAP has no standard way
/// to leave them with the server).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AutoReply {
    pub account: String,
    pub on: bool,
    /// Only between these dates (YYYY-MM-DD), when set.
    pub start: String,
    pub end: String,
    pub subject: String,
    pub text: String,
    /// Only to people you have written to (in Sent).
    pub known_only: bool,
    /// When it was switched on (RFC 3339, UTC): only mail after this is answered.
    pub since: String,
}

impl AutoReply {
    /// Is it in force on this day (YYYY-MM-DD)?
    pub fn active_on(&self, day: &str) -> bool {
        self.on
            && !self.text.trim().is_empty()
            && (self.start.trim().is_empty() || day >= self.start.trim())
            && (self.end.trim().is_empty() || day <= self.end.trim())
    }
}

/// An email signature for one account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Signature {
    pub account: String,
    pub text: String,
    /// Add it to new messages.
    pub new_messages: bool,
    /// Add it to replies and forwards.
    pub replies: bool,
}

/// A whole message to start from: its subject (if any) and text (Templates).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Template {
    pub name: String,
    pub subject: String,
    pub text: String,
}

/// A piece of text to drop into a message where the cursor is (Quick Parts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QuickPart {
    pub name: String,
    pub text: String,
}

/// Several actions under one button on the ribbon (Quick steps).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QuickStep {
    pub name: String,
    pub account: String,
    pub mark_read: bool,
    pub flag: bool,
    /// Category key.
    pub categorise: Option<String>,
    pub move_to: Option<String>,
    pub delete: bool,
}

/// A saved search shown as a folder (Search folders).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SearchFolder {
    pub name: String,
    pub account: String,
    pub words: String,
    pub unread_only: bool,
    pub flagged_only: bool,
    /// Category key.
    pub category: Option<String>,
}

/// Colour messages in the list that match (Conditional formatting).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CondFormat {
    pub name: String,
    pub on: bool,
    pub from: String,
    pub subject: String,
    pub unread_only: bool,
    pub colour: [u8; 3],
    pub bold: bool,
}

impl Default for CondFormat {
    fn default() -> Self {
        CondFormat {
            name: String::new(),
            on: true,
            from: String::new(),
            subject: String::new(),
            unread_only: false,
            colour: [0xE7, 0x48, 0x56],
            bold: false,
        }
    }
}

/// A standing Sweep for one sender in one folder: keep only the latest message, or only
/// messages newer than so many days; the others are deleted (to Trash) or moved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Sweep {
    pub account: String,
    pub folder: String,
    pub sender: String,
    pub keep_latest: bool,
    pub older_than_days: Option<u32>,
    /// Move them here instead of deleting.
    pub move_to: Option<String>,
}

/// The quick buttons shown on a message in the list while the mouse is over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HoverAction {
    Delete,
    Archive,
    Flag,
    ReadUnread,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// (account, the name people see on mail from it)
    pub names: Vec<(String, String)>,
    pub signatures: Vec<Signature>,
    pub date_style: DateStyle,
    pub time_24h: bool,
    /// Flash the taskbar when new mail arrives and MailCraft is not in front (no sound).
    pub alert_new_mail: bool,
    pub density: Density,
    pub mark_read: MarkRead,
    pub mark_read_secs: u32,
    pub pictures: Pictures,
    pub offline: Offline,
    pub show_favourites: bool,
    /// Show each folder's total, not its unread count.
    pub count_total: bool,
    /// Write and send plain text instead of HTML.
    pub plain_text: bool,
    pub always_bcc: bool,
    /// Remind me when a message mentions an attachment but has none.
    pub attach_reminder: bool,
    pub attach_warn_mb: u32,
    /// Senders whose mail goes straight to Junk (addresses or @domains).
    pub blocked: Vec<String>,
    /// Senders that are never blocked, and whose pictures may load.
    pub safe: Vec<String>,
    /// Check every account for new mail this often (minutes; 0: only when asked).
    pub check_minutes: u32,
    pub after_delete: AfterDelete,
    pub read_on_delete: bool,
    pub empty_trash_on_exit: bool,
    pub receipts: Receipts,
    pub templates: Vec<Template>,
    pub quick_parts: Vec<QuickPart>,
    pub quick_steps: Vec<QuickStep>,
    pub search_folders: Vec<SearchFolder>,
    pub formats: Vec<CondFormat>,
    pub sweeps: Vec<Sweep>,
    pub hover_actions: Vec<HoverAction>,
    /// Focused Inbox: newsletters and notifications go under Other.
    pub focused_inbox: bool,
    /// Senders always put in Focused, or in Other, whatever they send.
    pub focused_senders: Vec<String>,
    pub other_senders: Vec<String>,
    pub text_size: TextSize,
    pub rows: Rows,
    /// Rows was chosen in Settings (until then it follows Outlook's default, by width).
    pub rows_chosen: bool,
    /// One-row list: the columns shown (Subject always is), and From with the address.
    pub list_columns: Vec<ListCol>,
    pub list_addresses: bool,
    /// list_columns holds From and Date too (before, they were always shown). Missing from an
    /// older file means false (not the struct's default), so it is brought up to date.
    #[serde(default)]
    pub list_columns_v2: bool,
    /// list_columns is in display order and holds Subject (columns can be moved).
    #[serde(default)]
    pub list_columns_v3: bool,
    pub preview: Preview,
    pub subject_first: bool,
    pub animations: Animations,
    pub sender_images: bool,
    pub attachment_previews: bool,
    pub date_headers: bool,
    /// The reading pane shows the whole conversation, not just the selected message.
    pub conversation_reading: bool,
    /// Conversations include what is in Trash.
    pub conversation_deleted: bool,
    pub auto_replies: Vec<AutoReply>,
    /// Who has had an automatic reply: (account, sender, the reply's start date).
    pub auto_replied: Vec<(String, String, String)>,
    /// Notifications at all (Settings > Notifications).
    pub notifications: bool,
    /// New mail in the Inbox: a card at the bottom right of the screen (Betterbird's), shown
    /// even when MailCraft is behind other windows or minimised.
    pub notify_card: bool,
    /// New mail in other folders too (sorted there by rules or the server), not only the
    /// Inbox; never Junk, Trash, Sent, Drafts or Archive, and not folders left out from their
    /// right-click menu.
    pub notify_all_folders: bool,
    /// New mail in the Inbox: a banner in the window (when there is no card).
    pub notify_banner: bool,
    /// Only for mail that goes to Focused (with the Focused Inbox on).
    pub notify_focused_only: bool,
    /// Errors and confirmations stay on screen until dismissed (Settings > Accessibility).
    pub notices_stay: bool,
    pub key_set: KeySet,
    /// Letters over the ribbon when Alt (or F10) is pressed.
    pub keytips: bool,
    /// Suggest people (written to or heard from) in To, Cc and Bcc.
    pub suggest_people: bool,
    /// Never suggested.
    pub hidden_people: Vec<String>,
    /// Where a search looks to begin with.
    pub search_scope: SearchScope,
    /// Searching all folders includes Trash (Junk never).
    pub search_deleted: bool,
    /// Recent searches, newest first.
    pub search_history: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            names: vec![],
            signatures: vec![],
            date_style: DateStyle::DayMonth,
            time_24h: true,
            alert_new_mail: true,
            density: Density::Roomy,
            mark_read: MarkRead::OnOpen,
            mark_read_secs: 5,
            pictures: Pictures::Ask,
            offline: Offline::All,
            show_favourites: true,
            count_total: false,
            plain_text: false,
            always_bcc: false,
            attach_reminder: true,
            attach_warn_mb: 25,
            blocked: vec![],
            safe: vec![],
            check_minutes: 5,
            after_delete: AfterDelete::OpenNext,
            read_on_delete: false,
            empty_trash_on_exit: false,
            receipts: Receipts::Ask,
            templates: vec![],
            quick_parts: vec![],
            quick_steps: vec![],
            search_folders: vec![],
            formats: vec![],
            sweeps: vec![],
            hover_actions: vec![HoverAction::Delete, HoverAction::Flag, HoverAction::ReadUnread],
            focused_inbox: false,
            focused_senders: vec![],
            other_senders: vec![],
            text_size: TextSize::Medium,
            rows: Rows::BySize,
            rows_chosen: false,
            list_columns: vec![ListCol::From, ListCol::Subject, ListCol::Date],
            list_addresses: false,
            list_columns_v2: true,
            list_columns_v3: true,
            preview: Preview::Inline,
            subject_first: false,
            animations: Animations::System,
            sender_images: false,
            attachment_previews: true,
            date_headers: true,
            conversation_reading: true,
            conversation_deleted: true,
            auto_replies: vec![],
            auto_replied: vec![],
            notifications: true,
            notify_card: true,
            notify_all_folders: true,
            notify_banner: true,
            notify_focused_only: false,
            notices_stay: false,
            key_set: KeySet::Outlook,
            keytips: true,
            suggest_people: true,
            hidden_people: vec![],
            search_scope: SearchScope::FromInbox,
            search_deleted: true,
            search_history: vec![],
        }
    }
}

impl Settings {
    /// Remember a search (newest first, no repeats, the last 20).
    pub fn remember_search(&mut self, words: &str) {
        let w = words.trim();
        if w.chars().count() < 2 {
            return;
        }
        self.search_history.retain(|h| !h.eq_ignore_ascii_case(w));
        self.search_history.insert(0, w.to_string());
        self.search_history.truncate(20);
    }

    pub fn name_for(&self, account: &str) -> Option<String> {
        self.names.iter().find(|(a, _)| a == account).map(|(_, n)| n.clone()).filter(|n| !n.trim().is_empty())
    }

    pub fn signature_for(&self, account: &str) -> Option<&Signature> {
        self.signatures.iter().find(|s| s.account == account && !s.text.trim().is_empty())
    }

    /// Does an address match a list entry ("someone@example.com" or "@example.com" / "example.com")?
    pub fn listed(list: &[String], address: &str) -> bool {
        let a = address.trim().to_ascii_lowercase();
        list.iter().map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty()).any(|e| {
            if e.contains('@') && !e.starts_with('@') {
                a == e
            } else {
                let d = e.trim_start_matches('@');
                a.rsplit_once('@').is_some_and(|(_, ad)| ad == d || ad.ends_with(&format!(".{d}")))
            }
        })
    }
}

pub fn load() -> Settings {
    let mut s: Settings = crate::accounts::read_settings("settings.json").unwrap_or_default();
    // "Compact" was one-line rows before rows had their own setting.
    if s.density == Density::Compact {
        s.density = Density::Cozy;
        s.rows = Rows::One;
        s.rows_chosen = true;
    }
    if !s.rows_chosen {
        s.rows = Rows::BySize;
    }
    if !s.list_columns_v2 {
        for c in [ListCol::From, ListCol::Date] {
            if !s.list_columns.contains(&c) {
                s.list_columns.push(c);
            }
        }
        s.list_columns_v2 = true;
    }
    if !s.list_columns_v3 {
        // Into Betterbird's order, with Subject where it always was.
        let mut ordered = vec![];
        for c in ListCol::ALL {
            if s.list_columns.contains(&c) || c == ListCol::Subject {
                ordered.push(c);
            }
        }
        s.list_columns = ordered;
        s.list_columns_v3 = true;
    }
    if !s.list_columns.contains(&ListCol::Subject) {
        ListCol::insert_in_order(&mut s.list_columns, ListCol::Subject);
    }
    s
}

pub fn save(s: &Settings) -> std::io::Result<()> {
    crate::accounts::write_settings("settings.json", s)
}

impl CondFormat {
    /// Does a message match? Every filled-in condition must hold; at least one must be filled in.
    pub fn matches(&self, from: &str, subject: &str, unread: bool) -> bool {
        let words = |s: &str| s.split([';', ',']).map(|w| w.trim().to_lowercase()).filter(|w| !w.is_empty()).collect::<Vec<_>>();
        let (f, sj) = (words(&self.from), words(&self.subject));
        if !self.on || (f.is_empty() && sj.is_empty() && !self.unread_only) {
            return false;
        }
        let has = |hay: &str, ws: &[String]| {
            let hay = hay.to_lowercase();
            ws.iter().any(|w| hay.contains(w.as_str()))
        };
        (f.is_empty() || has(from, &f)) && (sj.is_empty() || has(subject, &sj)) && (!self.unread_only || unread)
    }
}

/// Does the message talk about an attachment? (For the forgotten-attachment reminder.)
pub fn mentions_attachment(subject: &str, body: &str) -> bool {
    let t = format!("{subject} {body}").to_lowercase();
    ["attached", "attachment", "i attach", "i've attached", "enclosed", "see attach", "find attach"].iter().any(|w| t.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_lists() {
        let list = vec!["spam@bad.example".to_string(), "@junk.example".into(), "promo.example".into()];
        assert!(Settings::listed(&list, "Spam@Bad.Example"));
        assert!(Settings::listed(&list, "anyone@junk.example"));
        assert!(Settings::listed(&list, "x@mail.promo.example"));
        assert!(!Settings::listed(&list, "good@bad.example"));
        assert!(!Settings::listed(&list, "x@notpromo.example"));
    }

    #[test]
    fn conditional_formats() {
        let c = CondFormat { from: "boss@work.example".into(), ..Default::default() };
        assert!(c.matches("The Boss <boss@work.example>", "anything", false));
        assert!(!c.matches("x@y.example", "anything", false));
        let u = CondFormat { subject: "invoice".into(), unread_only: true, ..Default::default() };
        assert!(u.matches("a", "Your INVOICE", true) && !u.matches("a", "Your invoice", false));
        assert!(!CondFormat::default().matches("a", "b", true));
    }

    #[test]
    fn list_columns_upgrade() {
        // A file from before From and Date were columns: they are added.
        let old: Settings = serde_json::from_str(r#"{"list_columns":["Flag"]}"#).unwrap();
        assert!(!old.list_columns_v2);
        let new: Settings = serde_json::from_str("{}").unwrap();
        assert!(new.list_columns.contains(&ListCol::From) && new.list_columns.contains(&ListCol::Date));
        // Moving and turning on.
        let mut l = vec![ListCol::From, ListCol::Subject, ListCol::Date];
        ListCol::move_to(&mut l, ListCol::Date, 0);
        assert_eq!(l, vec![ListCol::Date, ListCol::From, ListCol::Subject]);
        ListCol::move_to(&mut l, ListCol::Date, 3);
        assert_eq!(l, vec![ListCol::From, ListCol::Subject, ListCol::Date]);
        ListCol::insert_in_order(&mut l, ListCol::Flag);
        ListCol::insert_in_order(&mut l, ListCol::Size);
        // Betterbird's order: Size comes after Date.
        assert_eq!(l, vec![ListCol::Flag, ListCol::From, ListCol::Subject, ListCol::Date, ListCol::Size]);
    }

    #[test]
    fn search_history() {
        let mut s = Settings::default();
        for w in ["strap", "invoice", "Strap", "x"] {
            s.remember_search(w);
        }
        assert_eq!(s.search_history, vec!["Strap".to_string(), "invoice".to_string()]);
        for i in 0..30 {
            s.remember_search(&format!("search {i}"));
        }
        assert_eq!(s.search_history.len(), 20);
        assert_eq!(s.search_history.first().map(String::as_str), Some("search 29"));
    }

    #[test]
    fn auto_reply_dates() {
        let a = AutoReply { on: true, start: "2026-10-10".into(), end: "2026-10-20".into(), text: "Away".into(), ..Default::default() };
        assert!(!a.active_on("2026-10-09") && a.active_on("2026-10-10") && a.active_on("2026-10-20") && !a.active_on("2026-10-21"));
        assert!(AutoReply { on: true, text: "Away".into(), ..Default::default() }.active_on("2030-01-01"));
        assert!(!AutoReply { on: true, ..Default::default() }.active_on("2026-10-10"));
    }

    #[test]
    fn attachment_mentions() {
        assert!(mentions_attachment("Invoice", "Please find attached the invoice."));
        assert!(mentions_attachment("See attachment", ""));
        assert!(!mentions_attachment("Hello", "No files here."));
    }
}
