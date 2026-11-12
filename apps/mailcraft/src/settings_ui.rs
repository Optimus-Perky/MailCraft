//! The Settings window, laid out like Outlook's: Account, General and Email on the left, the
//! chosen one's sections beside them, the page on the right, and a search box. It is drawn in
//! the window's own theme. Changes apply at once and are saved as they are made.

use crate::looks::Theme;
use crate::ribbon::{PaneAt, SortKey};
use crate::settings::{
    AfterDelete, Animations, DateStyle, Density, KeySet, MarkRead, Pictures, Preview, Receipts, Rows, SearchScope, Signature, TextSize,
};
use crate::{Dialog, MailApp};
use egui::{Color32, Rect, RichText, Sense, Stroke, pos2, vec2};
use wordcraft_ui_egui::theme::{Tokens, regular, semibold};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    // Account (the account picked in the Account settings tree)
    Accounts,
    ServerSettings,
    AutoReplies,
    Signatures,
    Storage,
    Categories,
    SendOnly,
    Calendars,
    // General
    Language,
    Appearance,
    Notifications,
    Accessibility,
    Privacy,
    Search,
    // Email
    MessageList,
    ReadingPane,
    FolderPane,
    Compose,
    Attachments,
    Rules,
    Junk,
    Sync,
    Handling,
    Templates,
    QuickParts,
    QuickSteps,
    SearchFolders,
    Formatting,
    Sweep,
    Actions,
}

/// Changing an account's servers and sign-in (Settings > Email accounts > Edit). The address
/// itself stays: the mail kept here and the saved sign-in are filed under it.
pub struct AccountForm {
    pub index: usize,
    pub email: String,
    pub username: String,
    pub imap: crate::accounts::ServerCfg,
    pub smtp: crate::accounts::ServerCfg,
    /// Typed here, kept in Credential Manager on Save; empty keeps the saved one.
    pub password: String,
    pub oauth: bool,
    pub busy: bool,
    pub result: Option<Result<String, String>>,
}

impl AccountForm {
    fn from(index: usize, a: &crate::accounts::Account) -> Self {
        AccountForm {
            index,
            email: a.email.clone(),
            username: a.username.clone(),
            imap: a.imap.clone(),
            smtp: a.smtp.clone().unwrap_or(crate::accounts::ServerCfg {
                host: String::new(),
                port: 587,
                security: mailcraft_autoconfig::Security::StartTls,
            }),
            password: String::new(),
            oauth: a.oauth.is_some(),
            busy: false,
            result: None,
        }
    }

    /// The account as the form has it.
    fn apply(&self, a: &crate::accounts::Account) -> crate::accounts::Account {
        let mut a = a.clone();
        a.username = self.username.trim().to_string();
        a.imap = crate::accounts::ServerCfg { host: self.imap.host.trim().to_string(), ..self.imap.clone() };
        a.smtp =
            (!self.smtp.host.trim().is_empty()).then(|| crate::accounts::ServerCfg { host: self.smtp.host.trim().to_string(), ..self.smtp.clone() });
        a
    }

    fn problem(&self) -> Option<&'static str> {
        if self.imap.host.trim().is_empty() {
            Some("Type the incoming (IMAP) server.")
        } else if self.username.trim().is_empty() {
            Some("Type the user name (usually the address).")
        } else {
            None
        }
    }
}

/// Adding or changing a send-only address (Settings > Email accounts).
pub struct SenderForm {
    /// Which one is being changed; None for a new one.
    pub editing: Option<usize>,
    pub email: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub security: mailcraft_autoconfig::Security,
    pub username: String,
    /// Typed here and kept in Credential Manager on Save; empty keeps the saved one.
    pub password: String,
    /// Offered for this account ("" for all).
    pub account: String,
    pub allow_insecure: bool,
    /// Signs in through the provider (Outlook.com, Gmail) instead of with a password; and
    /// whether that was picked by hand (until then a provider's address defaults to it).
    pub oauth: Option<mailcraft_core::oauth::Provider>,
    pub auth_chosen: bool,
    /// Waiting for Find settings or Test.
    pub busy: bool,
    pub result: Option<Result<String, String>>,
}

impl SenderForm {
    fn new() -> Self {
        SenderForm {
            editing: None,
            email: String::new(),
            name: String::new(),
            host: String::new(),
            port: 587,
            security: mailcraft_autoconfig::Security::StartTls,
            username: String::new(),
            password: String::new(),
            account: String::new(),
            allow_insecure: false,
            oauth: None,
            auth_chosen: false,
            busy: false,
            result: None,
        }
    }

    fn from(i: usize, s: &crate::accounts::Sender) -> Self {
        SenderForm {
            editing: Some(i),
            email: s.email.clone(),
            name: s.name.clone(),
            host: s.smtp.host.clone(),
            port: s.smtp.port,
            security: s.smtp.security,
            username: s.username.clone(),
            password: String::new(),
            account: s.account.clone(),
            allow_insecure: s.allow_insecure,
            oauth: s.oauth,
            auth_chosen: true,
            busy: false,
            result: None,
        }
    }

    fn sender(&self) -> crate::accounts::Sender {
        crate::accounts::Sender {
            email: self.email.trim().to_string(),
            name: self.name.trim().to_string(),
            smtp: crate::accounts::ServerCfg { host: self.host.trim().to_string(), port: self.port, security: self.security },
            username: if self.username.trim().is_empty() { self.email.trim().to_string() } else { self.username.trim().to_string() },
            account: self.account.clone(),
            allow_insecure: self.allow_insecure,
            oauth: self.oauth,
        }
    }

    /// What is wrong with it, if anything.
    fn problem(&self) -> Option<&'static str> {
        let e = self.email.trim();
        if !e.contains('@') || e.starts_with('@') || e.ends_with('@') {
            Some("Type the address it sends from.")
        } else if self.host.trim().is_empty() {
            Some("Type its outgoing (SMTP) server, or press Find settings.")
        } else if self.port == 0 {
            Some("Type the server's port.")
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Account,
    General,
    Email,
}

impl Group {
    fn pages(self) -> &'static [Page] {
        match self {
            Group::Account => &[
                Page::Accounts,
                Page::ServerSettings,
                Page::AutoReplies,
                Page::Storage,
                Page::Categories,
                Page::SendOnly,
                Page::Calendars,
                Page::Signatures,
            ],
            Group::General => &[Page::Language, Page::Appearance, Page::Notifications, Page::Accessibility, Page::Privacy, Page::Search],
            // In Outlook's order.
            Group::Email => &[
                Page::MessageList,
                Page::ReadingPane,
                Page::FolderPane,
                Page::Compose,
                Page::Templates,
                Page::QuickParts,
                Page::Attachments,
                Page::Rules,
                Page::Formatting,
                Page::Sweep,
                Page::Junk,
                Page::QuickSteps,
                Page::SearchFolders,
                Page::Actions,
                Page::Sync,
                Page::Handling,
            ],
        }
    }

    fn of(p: Page) -> Group {
        [Group::Account, Group::General, Group::Email].into_iter().find(|g| g.pages().contains(&p)).unwrap_or(Group::Email)
    }
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Accounts => "Account settings",
            Page::ServerSettings => "Server settings",
            Page::SendOnly => "Send-only addresses",
            Page::Calendars => "Calendars",
            Page::AutoReplies => "Automatic replies",
            Page::Signatures => "Signatures",
            Page::Categories => "Categories",
            Page::Storage => "Storage",
            Page::Language => "Language and time",
            Page::Appearance => "Appearance",
            Page::Notifications => "Notifications",
            Page::Accessibility => "Accessibility",
            Page::Privacy => "Privacy and data",
            Page::Search => "Search",
            Page::MessageList => "Message list",
            Page::ReadingPane => "Reading pane",
            Page::FolderPane => "Folder pane",
            Page::Compose => "Compose",
            Page::Attachments => "Attachments",
            Page::Rules => "Rules",
            Page::Junk => "Junk email",
            Page::Sync => "Sync email",
            Page::Handling => "Message handling",
            Page::Templates => "Templates",
            Page::QuickParts => "Quick Parts",
            Page::QuickSteps => "Quick steps",
            Page::SearchFolders => "Search folders",
            Page::Formatting => "Conditional formatting",
            Page::Sweep => "Sweep",
            Page::Actions => "Customise actions",
        }
    }

    /// Words the search box finds the page by.
    fn words(self) -> &'static str {
        match self {
            Page::Accounts => "account email name display from remove add server send only smtp sender alias identity outgoing",
            Page::ServerSettings => "server settings imap smtp incoming outgoing port password security sign in",
            Page::SendOnly => "send only addresses identities smtp alias outgoing from",
            Page::Calendars => "calendars calendar caldav nextcloud events appointments sync server radicale",
            Page::AutoReplies => "automatic replies out of office away vacation holiday",
            Page::Signatures => "signature sign off",
            Page::Categories => "categories colour color tags labels",
            Page::Storage => "storage space quota size empty trash junk download cache",
            Page::Language => "language time date format clock 24 hour zone region",
            Page::Appearance => "appearance theme dark light high contrast mode",
            Page::Notifications => "notifications alert new mail flash taskbar sound banner focused",
            Page::Accessibility => {
                "accessibility animations text size bigger keyboard shortcuts gmail yahoo keytips alt contrast timing errors confirmations dismiss"
            }
            Page::Privacy => {
                "privacy data pictures images download external remote tracking read receipts suggested people contacts autocomplete hidden search history export"
            }
            Page::Search => "search scope all folders current inbox deleted trash results relevant top history",
            Page::MessageList => {
                "message list focused inbox other text size spacing conversations group preview rows layout sender images attachments date headers subject first animations density compact sort order deleted"
            }
            Page::ReadingPane => "reading pane position right bottom off mark read pictures images external dark background",
            Page::FolderPane => "folder pane favourites count unread total",
            Page::Compose => "compose format html plain text bcc signature",
            Page::Attachments => "attachments size reminder forgotten",
            Page::Rules => "rules automatic sort move",
            Page::Junk => "junk spam blocked safe senders block",
            Page::Sync => "sync check new mail minutes interval",
            Page::Handling => "message handling delete next previous trash empty exit read receipts",
            Page::Templates => "templates message start text subject",
            Page::QuickParts => "quick parts text snippet insert",
            Page::QuickSteps => "quick steps actions one click ribbon",
            Page::SearchFolders => "search folders saved virtual unread flagged",
            Page::Formatting => "conditional formatting colour color highlight",
            Page::Sweep => "sweep clean up sender keep latest older",
            Page::Actions => "customise customize actions hover buttons quick",
        }
    }
}

/// Small icons for the categories, drawn here (16 x 16 at `r`).
fn group_icon(p: &egui::Painter, r: Rect, g: Group, c: Color32, accent: Color32) {
    let at = |x: f32, y: f32| pos2(r.left() + x, r.top() + y);
    let s = Stroke::new(1.3, c);
    match g {
        Group::Account => {
            p.circle_stroke(at(8.0, 5.0), 3.2, s);
            p.add(egui::Shape::line(vec![at(2.0, 15.0), at(3.0, 11.5), at(5.5, 9.8), at(10.5, 9.8), at(13.0, 11.5), at(14.0, 15.0)], s));
        }
        Group::General => crate::ribbon::gear(p, r, c, accent),
        Group::Email => {
            p.rect_stroke(Rect::from_min_max(at(1.5, 3.5), at(14.5, 12.5)), 1.0, s, egui::StrokeKind::Middle);
            p.add(egui::Shape::line(vec![at(1.5, 4.0), at(8.0, 9.0), at(14.5, 4.0)], s));
        }
    }
}

/// One entry in a settings list, Outlook style: a bar in the accent colour beside the chosen one.
fn nav_item(ui: &mut egui::Ui, t: &Tokens, label: &str, on: bool, icon: Option<Group>, width: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 32.0), Sense::click());
    let p = ui.painter();
    if on {
        p.rect_filled(rect, 4.0, t.checked);
        p.rect_filled(Rect::from_min_size(rect.left_center() + vec2(0.0, -8.0), vec2(3.0, 16.0)), 1.5, t.accent);
    } else if resp.hovered() {
        p.rect_filled(rect, 4.0, t.hover);
    }
    let mut x = rect.left() + 14.0;
    if let Some(g) = icon {
        group_icon(p, Rect::from_min_size(pos2(x, rect.center().y - 8.0), vec2(16.0, 16.0)), g, if on { t.accent } else { t.icon }, t.accent);
        x += 28.0;
    }
    p.text(pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, label, if on { semibold(13.5) } else { regular(13.5) }, t.text);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// The one setting for pictures from the internet, for every account.
fn pictures(ui: &mut egui::Ui, p: &mut Pictures) {
    ui.radio_value(p, Pictures::Always, "Always download pictures");
    ui.radio_value(p, Pictures::SafeSenders, "Download them for safe senders only (Junk email), block the rest");
    ui.radio_value(p, Pictures::Ask, "Don't download pictures until I choose Show pictures");
}

/// 26666 as "26,666".
pub(crate) fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn mb(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", b / 1024.0 / 1024.0 / 1024.0)
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    } else {
        format!("{:.0} KB", (b / 1024.0).ceil())
    }
}

impl MailApp {
    /// The Settings tab (General and Email) or the Account settings tab, filling the window
    /// below the tabs, as Betterbird shows them.
    pub(crate) fn settings_view(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let Some(tab) = self.tabs.get(self.active_tab).cloned() else { return };
        let start_page = tab_page(&tab);
        let (accounts_tab, mut page, mut search) = match tab {
            crate::MainTab::Settings { page, search } => (false, page, search),
            crate::MainTab::Accounts { page, search } => (true, page, search),
            crate::MainTab::Mail | crate::MainTab::Calendar | crate::MainTab::Message { .. } => return,
        };
        let groups: &[Group] = if accounts_tab { &[Group::Account] } else { &[Group::General, Group::Email] };
        let mut other_tab: Option<Page> = None;
        let before = self.settings.clone();
        let cats_before = self.categories.clone();
        let full = ui.max_rect();
        let h = full.height();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
            // Left: the title, search, and the categories (Settings) or pages (Account settings);
            // at the foot, the way to the other tab.
            ui.allocate_ui_with_layout(vec2(240.0, h), egui::Layout::top_down(egui::Align::Min), |ui| {
                egui::Frame::NONE.inner_margin(egui::Margin { left: 14, right: 12, top: 16, bottom: 12 }).show(ui, |ui| {
                    ui.set_min_height(h - 30.0);
                    if accounts_tab {
                        // Betterbird's: Add account, then each account with its pages under it,
                        // then what is shared by them all.
                        let add = egui::Button::new(RichText::new("+  Add account").color(Color32::WHITE).font(semibold(13.0)))
                            .fill(t.accent)
                            .min_size(vec2(214.0, 30.0));
                        if ui.add(add).clicked() {
                            self.add = Some(crate::AddDialog::default());
                        }
                        ui.add_space(10.0);
                        let accounts: Vec<(String, Option<[u8; 3]>)> = self.accounts.iter().map(|a| (a.title().to_string(), a.colour)).collect();
                        for (i, (title, colour)) in accounts.iter().enumerate() {
                            let open = i == self.settings_acct;
                            let label = if i == 0 { format!("{title}  \u{2605}") } else { title.clone() };
                            let (rect, resp) = ui.allocate_exact_size(vec2(214.0, 30.0), Sense::click());
                            let on = open && page == Page::Accounts;
                            if on {
                                ui.painter().rect_filled(rect, 4.0, t.checked);
                                ui.painter().rect_filled(Rect::from_min_size(rect.left_center() + vec2(0.0, -8.0), vec2(3.0, 16.0)), 1.5, t.accent);
                            } else if resp.hovered() {
                                ui.painter().rect_filled(rect, 4.0, t.hover);
                            }
                            crate::foldertree::twisty(ui.painter(), rect.left_center() + vec2(12.0, 0.0), open, t.text_dim);
                            let tint = colour.map(|[r, g, b]| Color32::from_rgb(r, g, b));
                            crate::foldertree::icon_tinted(
                                ui.painter(),
                                Rect::from_min_size(rect.left_center() + vec2(22.0, -8.0), vec2(16.0, 16.0)),
                                None,
                                t.text,
                                t.accent,
                                tint,
                            );
                            let clip = ui.painter().with_clip_rect(rect.shrink(2.0));
                            clip.text(
                                rect.left_center() + vec2(44.0, 0.0),
                                egui::Align2::LEFT_CENTER,
                                label,
                                if on { semibold(13.0) } else { regular(13.0) },
                                tint.unwrap_or(t.text),
                            );
                            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                self.settings_acct = i;
                                page = Page::Accounts;
                                self.set_sel = None;
                            }
                            if open {
                                for p in [Page::ServerSettings, Page::AutoReplies, Page::Storage] {
                                    ui.horizontal(|ui| {
                                        ui.add_space(26.0);
                                        if nav_item(ui, t, p.title(), page == p, None, 188.0) && page != p {
                                            page = p;
                                            self.set_sel = None;
                                        }
                                    });
                                }
                            }
                        }
                        ui.add_space(10.0);
                        for p in [Page::Categories, Page::SendOnly, Page::Calendars] {
                            if nav_item(ui, t, p.title(), page == p, None, 214.0) && page != p {
                                page = p;
                                self.set_sel = None;
                            }
                        }
                    } else {
                        ui.label(RichText::new("Settings").font(semibold(20.0)).color(t.text));
                        ui.add_space(10.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut search)
                                .id(egui::Id::new("set_search"))
                                .hint_text("Find in Settings")
                                .desired_width(210.0),
                        );
                        ui.add_space(12.0);
                        for (label, g) in [("General", Group::General), ("Email", Group::Email)] {
                            let on = search.trim().is_empty() && Group::of(page) == g;
                            if nav_item(ui, t, label, on, Some(g), 214.0) {
                                if Group::of(page) != g {
                                    page = g.pages().first().copied().unwrap_or(Page::MessageList);
                                    self.set_sel = None;
                                }
                                search.clear();
                            }
                        }
                    }
                    // Betterbird keeps the way to the other settings at the bottom left.
                    let room = ui.available_height() - 40.0;
                    if room > 0.0 {
                        ui.add_space(room);
                    }
                    let (label, icon, to) = if accounts_tab {
                        ("Settings", Group::General, Page::MessageList)
                    } else {
                        ("Account settings", Group::Account, Page::Accounts)
                    };
                    if nav_item(ui, t, label, false, Some(icon), 214.0) {
                        other_tab = Some(to);
                    }
                });
            });
            let line = |ui: &mut egui::Ui| {
                let x = ui.cursor().left();
                ui.painter().line_segment([pos2(x, full.top()), pos2(x, full.bottom())], Stroke::new(1.0, t.border));
                ui.add_space(1.0);
            };
            line(ui);
            // Middle (Settings): the category's sections, or what the search found.
            let q = search.trim().to_lowercase();
            let found: Vec<Page> = groups
                .iter()
                .flat_map(|g| g.pages().iter().copied())
                .filter(|p| p.title().to_lowercase().contains(&q) || q.split_whitespace().all(|w| p.words().contains(w)))
                .collect();
            let listed: Vec<Page> = if !q.is_empty() {
                found
            } else if accounts_tab {
                vec![]
            } else {
                Group::of(page).pages().to_vec()
            };
            if !listed.is_empty() || !q.is_empty() {
                ui.allocate_ui_with_layout(vec2(216.0, h), egui::Layout::top_down(egui::Align::Min), |ui| {
                    egui::Frame::NONE.inner_margin(egui::Margin { left: 8, right: 8, top: 16, bottom: 8 }).show(ui, |ui| {
                        egui::ScrollArea::vertical().id_salt(("set_sections", accounts_tab)).max_height(h - 24.0).show(ui, |ui| {
                            if listed.is_empty() {
                                ui.label(RichText::new("Nothing matches.").color(t.text_dim));
                            }
                            for p in listed {
                                if nav_item(ui, t, p.title(), page == p, None, 196.0) && page != p {
                                    page = p;
                                    self.set_sel = None;
                                }
                            }
                        });
                    });
                });
                line(ui);
            }
            // Right: the page, at a width that reads well on a wide window.
            let w = ui.available_width();
            ui.allocate_ui_with_layout(vec2(w, h), egui::Layout::top_down(egui::Align::Min), |ui| {
                egui::Frame::NONE.inner_margin(egui::Margin { left: 26, right: 18, top: 16, bottom: 12 }).show(ui, |ui| {
                    ui.set_width((w - 44.0).min(900.0));
                    let title = match (page, self.accounts.get(self.settings_acct)) {
                        (Page::Accounts, Some(a)) => format!("Account settings - {}", a.email),
                        (Page::ServerSettings | Page::AutoReplies | Page::Storage, Some(a)) => format!("{} - {}", page.title(), a.email),
                        _ => page.title().to_string(),
                    };
                    ui.label(RichText::new(title).font(semibold(20.0)).color(t.text));
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical().id_salt(("set_page", page as u8)).auto_shrink([false, false]).max_height(h - 70.0).show(ui, |ui| {
                        ui.spacing_mut().item_spacing = vec2(8.0, 5.0);
                        self.settings_page(ui, &mut page, t);
                    });
                });
            });
        });
        if self.settings != before {
            crate::set_date_style(&self.settings);
            if before.pictures != self.settings.pictures || before.safe != self.settings.safe {
                self.restyle_message();
            }
            let s = &self.settings;
            let list_changed = before.density != s.density
                || before.rows != s.rows
                || before.preview != s.preview
                || before.date_headers != s.date_headers
                || before.focused_inbox != s.focused_inbox
                || before.focused_senders != s.focused_senders
                || before.other_senders != s.other_senders
                || before.conversation_deleted != s.conversation_deleted
                || before.search_deleted != s.search_deleted;
            if list_changed {
                self.refresh_messages();
            }
            if before.conversation_reading != self.settings.conversation_reading {
                self.refresh_thread();
            }
            if let Err(e) = crate::settings::save(&self.settings) {
                self.status = format!("Could not save settings: {e}");
            }
            if before.offline != self.settings.offline {
                self.start_download();
            }
        }
        if self.categories != cats_before
            && let Err(e) = crate::accounts::save_categories(&self.categories)
        {
            self.status = format!("Could not save the categories: {e}");
        }
        // A link to a page that lives in the other tab (Go to Signatures, Storage...) goes there,
        // and this tab stays where it was.
        let wrong_tab = (Group::of(page) == Group::Account) != accounts_tab;
        if wrong_tab {
            other_tab = Some(page);
            page = start_page;
        }
        if let Some(slot) = self.tabs.get_mut(self.active_tab) {
            *slot = if accounts_tab { crate::MainTab::Accounts { page, search } } else { crate::MainTab::Settings { page, search } };
        }
        if let Some(p) = other_tab {
            self.open_settings(p);
        }
    }

    /// Open the tab a settings page belongs to (Settings, or Account settings) at that page, or
    /// go to it if it is open.
    pub(crate) fn open_settings(&mut self, page: Page) {
        let accounts = Group::of(page) == Group::Account;
        // Signatures are on the account's own page.
        let page = if page == Page::Signatures { Page::Accounts } else { page };
        if accounts {
            self.quota = None;
            // Opened from the mail: about the account open there (unless already on one).
            if !self.tabs.iter().any(|t| matches!(t, crate::MainTab::Accounts { .. })) {
                self.settings_acct = self.account;
            }
        }
        let tab = if accounts {
            crate::MainTab::Accounts { page, search: String::new() }
        } else {
            crate::MainTab::Settings { page, search: String::new() }
        };
        match self
            .tabs
            .iter()
            .position(|t| matches!((t, accounts), (crate::MainTab::Accounts { .. }, true) | (crate::MainTab::Settings { .. }, false)))
        {
            Some(i) => {
                if let Some(slot) = self.tabs.get_mut(i) {
                    *slot = tab;
                }
                self.active_tab = i;
            }
            None => {
                self.tabs.push(tab);
                self.active_tab = self.tabs.len() - 1;
            }
        }
        self.set_sel = None;
    }

    fn settings_page(&mut self, ui: &mut egui::Ui, page: &mut Page, t: &Tokens) {
        let heading = |ui: &mut egui::Ui, s: &str| {
            ui.add_space(10.0);
            ui.label(RichText::new(s).font(semibold(14.5)).color(t.text));
            ui.add_space(2.0);
        };
        let note = |ui: &mut egui::Ui, s: &str| {
            ui.label(RichText::new(s).font(regular(11.5)).color(t.text_dim));
        };
        // Account pages are about the account picked in the tree; the rest, the one open.
        let mine = if Group::of(*page) == Group::Account {
            self.accounts.get(self.settings_acct).or(self.current()).map(|a| a.email.clone()).unwrap_or_default()
        } else {
            self.current().map(|a| a.email.clone()).unwrap_or_default()
        };
        let mine_acct = self.accounts.iter().find(|a| a.email == mine).cloned();
        let mut export = false;
        match *page {
            Page::Accounts => {
                let i = self.settings_acct;
                let Some(acct) = self.accounts.get(i).cloned() else {
                    note(ui, "No accounts yet: Add account, at the top left.");
                    return;
                };
                let mut a = acct.clone();
                // The account's own buttons, as Betterbird has them.
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(i > 0, egui::Button::new("Set as default"))
                        .on_hover_text("Opened when MailCraft starts; first in the folder list")
                        .clicked()
                    {
                        self.set_default_account(i);
                    }
                    let delete = egui::Button::new(RichText::new("Delete").color(Color32::WHITE)).fill(Color32::from_rgb(0xD9, 0x3D, 0x3D));
                    if ui.add(delete).on_hover_text("Remove the account from MailCraft (the mail stays on the server)").clicked() {
                        self.dialog = Some(Dialog::RemoveAccount(i));
                    }
                    if i == 0 {
                        ui.label(RichText::new("\u{2605} Default account").color(t.orange));
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_sized([130.0, 20.0], egui::Label::new("Account name"));
                    ui.add_sized(
                        [300.0, 22.0],
                        egui::TextEdit::singleline(&mut a.label).id(egui::Id::new(("acc_label", i))).hint_text(acct.email.as_str()),
                    );
                    ui.label("Colour");
                    let mut c = a.colour.unwrap_or([0x00, 0x78, 0xD4]);
                    if ui.color_edit_button_srgb(&mut c).changed() {
                        a.colour = Some(c);
                    }
                    if a.colour.is_some() && ui.small_button("Reset").clicked() {
                        a.colour = None;
                    }
                });
                heading(ui, "Default identity");
                note(ui, "What other people see when they read your messages.");
                let s = &mut self.settings;
                let at = match s.names.iter().position(|(x, _)| *x == a.email) {
                    Some(k) => k,
                    None => {
                        s.names.push((a.email.clone(), String::new()));
                        s.names.len() - 1
                    }
                };
                let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
                    ui.horizontal(|ui| {
                        ui.add_sized([130.0, 20.0], egui::Label::new(label));
                        add(ui);
                    });
                };
                row(ui, "Your name", &mut |ui| {
                    if let Some((_, n)) = s.names.get_mut(at) {
                        ui.add_sized([420.0, 22.0], egui::TextEdit::singleline(n).id(egui::Id::new(("set_name", i))).hint_text("e.g. Sam Taylor"));
                    }
                });
                row(ui, "Email address", &mut |ui| {
                    ui.label(RichText::new(&a.email).color(t.text));
                });
                row(ui, "Reply-to address", &mut |ui| {
                    ui.add_sized(
                        [420.0, 22.0],
                        egui::TextEdit::singleline(&mut a.reply_to)
                            .id(egui::Id::new(("acc_replyto", i)))
                            .hint_text("Recipients will reply to this other address"),
                    );
                });
                row(ui, "Organisation", &mut |ui| {
                    ui.add_sized([420.0, 22.0], egui::TextEdit::singleline(&mut a.organisation).id(egui::Id::new(("acc_org", i))));
                });
                let si = match s.signatures.iter().position(|x| x.account == a.email) {
                    Some(k) => k,
                    None => {
                        s.signatures.push(Signature { account: a.email.clone(), new_messages: true, replies: true, ..Default::default() });
                        s.signatures.len() - 1
                    }
                };
                if let Some(sig) = s.signatures.get_mut(si) {
                    ui.add_space(4.0);
                    ui.label("Signature text");
                    ui.add(
                        egui::TextEdit::multiline(&mut sig.text)
                            .id(egui::Id::new(("set_sig", a.email.as_str())))
                            .desired_width(ui.available_width().min(560.0))
                            .desired_rows(4)
                            .hint_text("Kind regards,\nMark"),
                    );
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut sig.new_messages, "Add it to new messages");
                        ui.checkbox(&mut sig.replies, "Add it to replies and forwards");
                    });
                }
                ui.add_space(6.0);
                row(ui, "Outgoing server", &mut |ui| {
                    let smtp = a.smtp.as_ref().map(|o| format!("{}:{}", o.host, o.port)).unwrap_or_else(|| "none".into());
                    ui.label(RichText::new(smtp).color(t.text_dim));
                    if ui.button("Edit outgoing server...").clicked() {
                        *page = Page::ServerSettings;
                    }
                });
                ui.add_space(6.0);
                if ui.button("Manage identities...").on_hover_text("Other addresses to send from (send-only addresses)").clicked() {
                    *page = Page::SendOnly;
                }
                if a != acct
                    && let Some(slot) = self.accounts.get_mut(i)
                {
                    *slot = a;
                    if let Err(e) = crate::accounts::save(&self.accounts) {
                        self.status = format!("Could not save the account: {e}");
                    }
                }
            }
            Page::ServerSettings => {
                let i = self.settings_acct;
                if self.account_form.as_ref().is_none_or(|f| f.index != i)
                    && let Some(a) = self.accounts.get(i)
                {
                    self.account_form = Some(AccountForm::from(i, a));
                }
                note(ui, &format!("For {mine}. The address itself cannot change: the mail kept here and the sign-in are filed under it."));
                self.account_form_ui(ui, t);
            }
            Page::SendOnly => {
                self.senders_section(ui, t);
            }
            Page::Calendars => {
                self.calendars_section(ui, t);
            }
            Page::AutoReplies => {
                note(ui, &format!("For {mine}."));
                let s = &mut self.settings;
                let at = match s.auto_replies.iter().position(|a| a.account == mine) {
                    Some(i) => i,
                    None => {
                        s.auto_replies.push(crate::settings::AutoReply { account: mine.clone(), ..Default::default() });
                        s.auto_replies.len() - 1
                    }
                };
                let Some(ar) = s.auto_replies.get_mut(at) else { return };
                let was_on = ar.on;
                ui.checkbox(&mut ar.on, RichText::new("Turn on automatic replies").font(semibold(13.0)));
                if ar.on && !was_on {
                    // A fresh start: only mail from now on, and everyone may have one again.
                    ar.since = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
                    s.auto_replied.retain(|(a, _, _)| *a != mine);
                }
                let Some(ar) = s.auto_replies.get_mut(at) else { return };
                ui.add_enabled_ui(ar.on, |ui| {
                    let mut period = !ar.start.is_empty() || !ar.end.is_empty();
                    if ui.checkbox(&mut period, "Send replies only during a time period").changed() {
                        if period {
                            let today = chrono::Local::now().date_naive();
                            ar.start = today.format("%Y-%m-%d").to_string();
                            ar.end = (today + chrono::Duration::days(7)).format("%Y-%m-%d").to_string();
                        } else {
                            ar.start.clear();
                            ar.end.clear();
                        }
                    }
                    if period {
                        ui.horizontal(|ui| {
                            ui.add_sized([60.0, 20.0], egui::Label::new("From"));
                            ui.add(egui::TextEdit::singleline(&mut ar.start).desired_width(110.0).hint_text("2026-10-20"));
                            ui.label("to");
                            ui.add(egui::TextEdit::singleline(&mut ar.end).desired_width(110.0).hint_text("2026-10-27"));
                            ui.label(RichText::new("(year-month-day, both days included)").font(regular(11.5)).color(t.text_dim));
                        });
                    }
                    field(ui, "Subject", &mut ar.subject);
                    note(ui, "Left empty: \"Automatic reply:\" and the subject of their message.");
                    ui.add(
                        egui::TextEdit::multiline(&mut ar.text)
                            .id(egui::Id::new("set_ar_text"))
                            .desired_width(ui.available_width().min(520.0))
                            .desired_rows(6)
                            .hint_text("I'm away until Monday 27 October, with limited access to email."),
                    );
                    ui.checkbox(&mut ar.known_only, "Only send replies to people I have written to");
                });
                ui.add_space(6.0);
                note(
                    ui,
                    "MailCraft sends these itself, so only while it is open. Each sender gets one reply; mailing lists, newsletters and other automatic mail never get one.",
                );
            }
            Page::Signatures => {
                // Signatures are on each account's page now, as in Betterbird.
                *page = Page::Accounts;
            }
            Page::Categories => {
                note(ui, "Click a colour to change it. Names can be changed and stay on the messages.");
                ui.add_space(4.0);
                let mut remove = None;
                // A row each, the name box a fixed size (in a grid it would take the column's
                // width from the frame before, and start as a sliver).
                for (i, c) in self.categories.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.color_edit_button_srgb(&mut c.colour);
                        ui.add_sized([260.0, 22.0], egui::TextEdit::singleline(&mut c.name).id(egui::Id::new(("set_cat", i))));
                        if ui.small_button("Delete").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    self.categories.remove(i);
                }
                if ui.button("New category").clicked() {
                    let base = crate::accounts::keyword("New category");
                    let mut key = base.clone();
                    let mut n = 2;
                    while self.categories.iter().any(|c| c.key.eq_ignore_ascii_case(&key)) {
                        key = format!("{base}_{n}");
                        n += 1;
                    }
                    self.categories.push(crate::accounts::Category { name: "New category".into(), colour: [0x00, 0x78, 0xD4], key });
                }
            }
            Page::Storage => {
                note(ui, &format!("For {mine}."));
                // The server's figure, asked for once each time the page opens.
                let asked = self.quota.as_ref().is_some_and(|(a, _)| *a == mine);
                if !asked
                    && let Some(acct) = mine_acct.clone()
                    && let Some(pw) = crate::secrets::get(&acct.email)
                {
                    self.quota = Some((mine.clone(), Ok(None)));
                    let _ = self.worker.send(crate::worker::Request::Quota(acct, pw));
                }
                heading(ui, "On the server");
                match self.quota.as_ref().map(|(_, q)| q) {
                    Some(Ok(Some((used, limit)))) if *limit > 0 => {
                        let f = (*used as f32 / *limit as f32).clamp(0.0, 1.0);
                        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width().min(420.0), 10.0), Sense::hover());
                        ui.painter().rect_filled(r, 5.0, t.hover);
                        ui.painter().rect_filled(
                            Rect::from_min_size(r.min, vec2(r.width() * f, r.height())),
                            5.0,
                            if f > 0.9 { t.red } else { t.accent },
                        );
                        ui.label(format!("{} of {} used ({:.0}%)", mb(*used as i64 * 1024), mb(*limit as i64 * 1024), f * 100.0));
                    }
                    Some(Err(e)) => note(ui, &format!("The server could not be asked: {e}")),
                    _ => note(ui, "The server does not say how much space the account has (or has not answered yet)."),
                }
                let usage = self.store.as_ref().and_then(|s| s.folder_usage(&mine).ok()).unwrap_or_default();
                heading(ui, "Folders");
                egui::Grid::new("set_storage").num_columns(4).striped(true).spacing(vec2(18.0, 4.0)).show(ui, |ui| {
                    ui.label(RichText::new("Folder").font(semibold(12.0)));
                    ui.label(RichText::new("Messages").font(semibold(12.0)));
                    ui.label(RichText::new("Size").font(semibold(12.0)));
                    ui.label("");
                    ui.end_row();
                    for (name, n, size, _) in &usage {
                        let kind = self.kind_of(name);
                        ui.label(crate::folder_label(name, &self.folders));
                        ui.label(n.to_string());
                        ui.label(mb(*size));
                        if matches!(kind, crate::foldertree::Kind::Trash | crate::foldertree::Kind::Junk) && *n > 0 {
                            let sure = self.set_confirm.as_deref() == Some(name.as_str());
                            if ui.button(if sure { "Click again to empty it" } else { "Empty" }).clicked() {
                                if sure {
                                    self.set_confirm = None;
                                    if let Some(acct) = mine_acct.clone()
                                        && let Some(pw) = crate::secrets::get(&acct.email)
                                    {
                                        let op = crate::worker::FolderOp::Empty(name.clone());
                                        self.folder_op_here(&acct.email, &op);
                                        let _ = self.worker.send(crate::worker::Request::Folder(acct, pw, op));
                                    }
                                } else {
                                    self.set_confirm = Some(name.clone());
                                }
                            }
                        } else {
                            ui.label("");
                        }
                        ui.end_row();
                    }
                });
                heading(ui, "On this PC");
                note(ui, "Mail kept here can be read offline, and search finds the words in it at once, without asking the server.");
                let o = &mut self.settings.offline;
                ui.radio_value(o, crate::settings::Offline::All, "Download all mail");
                ui.radio_value(o, crate::settings::Offline::Year, "Download mail from the last 12 months");
                ui.radio_value(o, crate::settings::Offline::Opened, "Only keep messages I open");
                if let Some((n, all)) = self.store.as_ref().and_then(|s| s.downloaded(&mine, None).ok()) {
                    ui.label(format!("{} of {} messages on this PC.", thousands(n), thousands(all)));
                }
                let here: i64 = usage.iter().map(|u| u.3).sum();
                ui.label(format!("Space they take: {}", mb(here)));
                if here > 0 && ui.button("Clear downloaded messages").clicked() {
                    if let Some(s) = &self.store {
                        let _ = s.forget_bodies(&mine);
                    }
                    self.status = "Downloaded copies cleared; they download again as set above.".into();
                }
                note(ui, "Headers stay so the lists still work, and so does the search index.");
            }
            Page::Language => {
                let s = &mut self.settings;
                heading(ui, "Language");
                ui.label("English (United Kingdom)");
                note(ui, "MailCraft is in UK English for now.");
                heading(ui, "Date format");
                ui.radio_value(&mut s.date_style, DateStyle::DayMonth, "01/09/2027 (day first)");
                ui.radio_value(&mut s.date_style, DateStyle::MonthDay, "09/01/2027 (month first)");
                ui.radio_value(&mut s.date_style, DateStyle::Iso, "2027-09-01");
                heading(ui, "Time format");
                ui.radio_value(&mut s.time_24h, true, "01:01 - 23:59");
                ui.radio_value(&mut s.time_24h, false, "1:01 AM - 11:59 PM");
                heading(ui, "Time zone");
                let now = chrono::Local::now();
                ui.label(format!("UTC{} (now {})", now.format("%:z"), now.format("%H:%M")));
                note(ui, "MailCraft uses the time zone Windows is set to.");
            }
            Page::Appearance => {
                heading(ui, "Theme");
                for (th, label) in [
                    (Theme::System, "Use the system setting"),
                    (Theme::Light, "Light"),
                    (Theme::Dark, "Dark"),
                    (Theme::ContrastBlack, "High contrast black"),
                    (Theme::ContrastWhite, "High contrast white"),
                ] {
                    ui.radio_value(&mut self.theme, th, label);
                }
                heading(ui, "Dark mode");
                let mut dark = self.dark_messages;
                if ui.checkbox(&mut dark, "Show messages with a dark background in the dark theme").changed() {
                    self.dark_messages = dark;
                    self.restyle_message();
                }
                heading(ui, "Text size");
                text_size(ui, &mut self.settings.text_size);
            }
            Page::Notifications => {
                let s = &mut self.settings;
                egui::Frame::group(ui.style()).fill(t.panel).stroke(Stroke::new(1.0, t.border)).corner_radius(6.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.checkbox(&mut s.notifications, RichText::new("Notifications in MailCraft").font(semibold(13.5)));
                    note(ui, "Tell me about new mail and other things happening in MailCraft.");
                });
                heading(ui, "Notify me about");
                ui.add_enabled_ui(s.notifications, |ui| {
                    egui::Frame::group(ui.style()).fill(t.panel).stroke(Stroke::new(1.0, t.border)).corner_radius(6.0).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("Mail").font(semibold(13.5)));
                        note(ui, "New email in the Inbox.");
                        ui.checkbox(
                            &mut s.notify_all_folders,
                            "And in other folders mail goes straight to (not Junk, Trash, Sent, Drafts or Archive)",
                        );
                        note(ui, "Leave a folder out with \"Notify about new mail here\" in its right-click menu.");
                        ui.add_space(4.0);
                        ui.checkbox(&mut s.notify_card, "Show a card at the bottom right of the screen, even when MailCraft is minimised");
                        note(ui, "As Betterbird's: who it is from, the subject and the start of the message. Click it to read the message.");
                        ui.add_enabled_ui(!s.notify_card, |ui| {
                            ui.checkbox(&mut s.notify_banner, "Otherwise, a banner inside MailCraft (bottom left)");
                        });
                        ui.checkbox(&mut s.alert_new_mail, "Flash MailCraft in the taskbar when it is behind other windows");
                        ui.add_enabled_ui(s.focused_inbox, |ui| {
                            ui.checkbox(&mut s.notify_focused_only, "Only for mail that goes to Focused");
                        });
                    });
                });
                note(ui, "MailCraft never plays a sound.");
            }
            Page::Accessibility => {
                let s = &mut self.settings;
                heading(ui, "Keyboard shortcuts");
                note(ui, "Choose which set of shortcuts to use, or turn them off.");
                ui.radio_value(&mut s.key_set, KeySet::Outlook, "Outlook (Ctrl+R, Ctrl+N...)");
                ui.radio_value(&mut s.key_set, KeySet::OutlookWeb, "Outlook on the web (R, N, E...)");
                ui.radio_value(&mut s.key_set, KeySet::Gmail, "Gmail");
                ui.radio_value(&mut s.key_set, KeySet::Yahoo, "Yahoo! Mail");
                ui.radio_value(&mut s.key_set, KeySet::Off, "Turn off keyboard shortcuts");
                ui.add_space(4.0);
                egui::CollapsingHeader::new("The shortcuts in this set").id_salt("set_keys_list").show(ui, |ui| {
                    egui::Grid::new("set_keys").num_columns(2).spacing(vec2(18.0, 3.0)).show(ui, |ui| {
                        for (k, what) in crate::shortcut_list(s.key_set) {
                            ui.label(RichText::new(k).font(semibold(12.0)));
                            ui.label(what);
                            ui.end_row();
                        }
                    });
                });
                heading(ui, "Keytips");
                note(ui, "Letters over the ribbon for getting about it with the keyboard.");
                ui.checkbox(&mut s.keytips, "Show keytips when I press the Alt key (or F10)");
                heading(ui, "Adjust timing");
                note(ui, "Errors and confirmations (moved, sent...) can stay on screen until you close them.");
                ui.checkbox(&mut s.notices_stay, "Show errors and confirmations until I dismiss them");
                heading(ui, "Text size");
                text_size(ui, &mut s.text_size);
                heading(ui, "Animations");
                animations(ui, &mut s.animations);
                heading(ui, "High contrast");
                note(ui, "Two high-contrast themes are under General > Appearance.");
            }
            Page::Privacy => {
                let s = &mut self.settings;
                heading(ui, "Suggested people");
                note(
                    ui,
                    "As you type a name in To, Cc or Bcc, MailCraft suggests people you've emailed or who have emailed you (not newsletters or Junk). It works from the mail on this PC; nothing is sent anywhere.",
                );
                ui.checkbox(&mut s.suggest_people, "Suggest people as I type");
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if ui.button("Clear data and turn off").on_hover_text("Turn suggestions off and forget who you hid").clicked() {
                        s.suggest_people = false;
                        s.hidden_people.clear();
                    }
                    if ui.button(if self.set_show_hidden { "Hide hidden contacts" } else { "Manage hidden contacts" }).clicked() {
                        self.set_show_hidden = !self.set_show_hidden;
                    }
                });
                if self.set_show_hidden {
                    if s.hidden_people.is_empty() {
                        note(ui, "Nobody is hidden. Press the cross beside a suggestion to hide someone.");
                    }
                    let mut back = None;
                    for (i, h) in s.hidden_people.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(h);
                            if ui.small_button("Suggest again").clicked() {
                                back = Some(i);
                            }
                        });
                    }
                    if let Some(i) = back {
                        s.hidden_people.remove(i);
                    }
                }
                heading(ui, "Search history");
                note(ui, "MailCraft keeps your last 20 searches to offer them again under the search box. They stay on this PC.");
                ui.horizontal(|ui| {
                    if ui.add_enabled(!s.search_history.is_empty(), egui::Button::new("Delete history")).clicked() {
                        s.search_history.clear();
                    }
                    if ui.add_enabled(!s.search_history.is_empty(), egui::Button::new("Export")).clicked() {
                        export = true;
                    }
                });
                heading(ui, "Pictures from the internet");
                note(ui, "For every account. Pictures from the internet can tell the sender when you read a message.");
                pictures(ui, &mut s.pictures);
                heading(ui, "Read receipts");
                note(ui, "When a message asks for one:");
                ui.radio_value(&mut s.receipts, Receipts::Ask, "Ask me");
                ui.radio_value(&mut s.receipts, Receipts::Always, "Always send one");
                ui.radio_value(&mut s.receipts, Receipts::Never, "Never send one");
                heading(ui, "Downloaded mail");
                if ui.button("Storage...").clicked() {
                    *page = Page::Storage;
                }
                if export {
                    self.export_search_history();
                }
            }
            Page::Search => {
                let s = &mut self.settings;
                heading(ui, "Search scope");
                let scope = s.search_scope;
                ui.radio_value(&mut s.search_scope, SearchScope::FromInbox, "All folders when searching from the Inbox");
                ui.radio_value(&mut s.search_scope, SearchScope::All, "All folders");
                ui.radio_value(&mut s.search_scope, SearchScope::Current, "Current folder");
                if s.search_scope != scope {
                    // Applied to the open folder at once.
                    self.scope_for = None;
                }
                note(ui, "The All folders box over the list changes it for one search.");
                let s = &mut self.settings;
                heading(ui, "Search results");
                ui.checkbox(&mut s.search_deleted, "Include deleted items");
                note(
                    ui,
                    "Junk is never searched across folders. Typing searches what is on this PC; Enter also searches the whole text on the server.",
                );
                heading(ui, "Search history");
                note(ui, "Your recent searches are offered under the search box.");
                if ui.button("Privacy and data...").clicked() {
                    *page = Page::Privacy;
                }
            }
            Page::MessageList => {
                let s = &mut self.settings;
                heading(ui, "Focused Inbox");
                ui.checkbox(&mut s.focused_inbox, "Sort messages into Focused and Other");
                note(
                    ui,
                    "Other gets newsletters, mailing lists and notifications, unless you have written to the sender. Right-click a message to always move a sender.",
                );
                heading(ui, "Text size and spacing");
                text_size(ui, &mut s.text_size);
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.radio_value(&mut s.density, Density::Roomy, "Roomy");
                    ui.radio_value(&mut s.density, Density::Cozy, "Cozy");
                });
                heading(ui, "Message organisation");
                ui.label("How do you want the message list organised?");
                let mut conv = self.threads;
                ui.radio_value(&mut conv, true, "Group messages by conversation");
                ui.radio_value(&mut conv, false, "Do not group messages");
                if conv != self.threads {
                    self.threads = conv;
                    self.conv_folders.clear();
                    self.head = None;
                    self.arrange();
                }
                note(ui, "Folders can have their own setting: View > Show as conversations > This folder.");
                ui.add_space(4.0);
                ui.label("How do you want the reading pane organised?");
                let s = &mut self.settings;
                ui.add_enabled_ui(self.threads, |ui| {
                    ui.radio_value(&mut s.conversation_reading, true, "Show all messages from the selected conversation");
                    ui.radio_value(&mut s.conversation_reading, false, "Show only the selected message");
                });
                ui.add_space(4.0);
                ui.checkbox(&mut s.conversation_deleted, "Show deleted items in conversations");
                heading(ui, "Layout");
                ui.label("How many rows should each message use?");
                let rows = s.rows;
                ui.radio_value(&mut s.rows, Rows::Multiple, "Multiple rows");
                ui.radio_value(&mut s.rows, Rows::One, "One row, with column headings");
                ui.radio_value(&mut s.rows, Rows::BySize, "Switch based on the list's width");
                if s.rows != rows {
                    s.rows_chosen = true;
                }
                ui.add_space(4.0);
                ui.label("Show preview text?");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut s.preview, Preview::Off, "Off");
                    ui.radio_value(&mut s.preview, Preview::Inline, "Inline");
                    ui.radio_value(&mut s.preview, Preview::OneLine, "1 line");
                    ui.radio_value(&mut s.preview, Preview::TwoLines, "2 lines");
                });
                ui.add_space(4.0);
                ui.label("How should messages be displayed?");
                ui.radio_value(&mut s.subject_first, false, "Sender name first");
                ui.radio_value(&mut s.subject_first, true, "Subject first");
                ui.add_space(4.0);
                ui.label("Animations in the message list");
                animations(ui, &mut s.animations);
                heading(ui, "Personalise the message list");
                ui.checkbox(&mut s.sender_images, "Show sender pictures (their initials)");
                ui.checkbox(&mut s.attachment_previews, "Show attachments under messages");
                ui.checkbox(&mut s.date_headers, "Show date headers");
                heading(ui, "Order");
                let mut sort = self.sort;
                let mut reverse = self.reverse;
                ui.horizontal(|ui| {
                    ui.label("Sort by");
                    egui::ComboBox::from_id_salt("set_sort")
                        .selected_text(match sort {
                            SortKey::Date => "Date",
                            SortKey::From => "From",
                            SortKey::Subject => "Subject",
                            SortKey::Size => "Size",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut sort, SortKey::Date, "Date");
                            ui.selectable_value(&mut sort, SortKey::From, "From");
                            ui.selectable_value(&mut sort, SortKey::Subject, "Subject");
                            ui.selectable_value(&mut sort, SortKey::Size, "Size");
                        });
                    ui.checkbox(&mut reverse, "Reverse order");
                });
                if sort != self.sort || reverse != self.reverse {
                    self.sort = sort;
                    self.reverse = reverse;
                    self.refresh_messages();
                }
            }
            Page::ReadingPane => {
                let s = &mut self.settings;
                heading(ui, "Position");
                ui.radio_value(&mut self.pane_at, PaneAt::Right, "On the right");
                ui.radio_value(&mut self.pane_at, PaneAt::Bottom, "At the bottom");
                ui.radio_value(&mut self.pane_at, PaneAt::Off, "Off");
                heading(ui, "Mark as read");
                ui.radio_value(&mut s.mark_read, MarkRead::OnOpen, "When the message is opened");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut s.mark_read, MarkRead::AfterSeconds, "After it has been open for");
                    ui.add(egui::DragValue::new(&mut s.mark_read_secs).range(1..=120));
                    ui.label("seconds");
                });
                ui.radio_value(&mut s.mark_read, MarkRead::Never, "Don't mark it read automatically");
                heading(ui, "Pictures from the internet");
                pictures(ui, &mut s.pictures);
                note(ui, "The same setting as General > Privacy and data, for every account.");
                heading(ui, "Dark mode");
                let mut dark = self.dark_messages;
                if ui.checkbox(&mut dark, "Show messages with a dark background in the dark theme").changed() {
                    self.dark_messages = dark;
                    self.restyle_message();
                }
            }
            Page::FolderPane => {
                let s = &mut self.settings;
                heading(ui, "Show");
                ui.checkbox(&mut self.show_folders, "Folder pane");
                ui.checkbox(&mut s.show_favourites, "Favourites at the top");
                heading(ui, "Count");
                ui.radio_value(&mut s.count_total, false, "Unread messages");
                ui.radio_value(&mut s.count_total, true, "All messages");
            }
            Page::Compose => {
                let s = &mut self.settings;
                heading(ui, "Message format");
                ui.radio_value(&mut s.plain_text, false, "HTML (formatting, pictures, links)");
                ui.radio_value(&mut s.plain_text, true, "Plain text");
                heading(ui, "Fields");
                ui.checkbox(&mut s.always_bcc, "Always show Bcc");
                heading(ui, "Signature");
                note(ui, "Signatures are set per account, under Account.");
                if ui.button("Go to Signatures").clicked() {
                    *page = Page::Signatures;
                }
            }
            Page::Attachments => {
                let s = &mut self.settings;
                heading(ui, "Reminder");
                ui.checkbox(&mut s.attach_reminder, "Remind me if a message mentions an attachment but has none");
                heading(ui, "Size");
                ui.horizontal(|ui| {
                    ui.label("Warn when attachments add up to more than");
                    ui.add(egui::DragValue::new(&mut s.attach_warn_mb).range(1..=500));
                    ui.label("MB");
                });
                note(ui, "Most mail servers refuse messages over 25 MB.");
            }
            Page::Rules => {
                let mine = self.current().map(|a| a.email.clone()).unwrap_or_default();
                let rules: Vec<(bool, String, String)> =
                    self.rules.iter().filter(|r| r.account == mine).map(|r| (r.enabled, r.name.clone(), r.summary())).collect();
                if rules.is_empty() {
                    ui.label(RichText::new("No rules for this account yet.").color(t.text_dim));
                }
                for (on, name, summary) in rules {
                    ui.label(RichText::new(format!("{}{name}", if on { "" } else { "(off) " })).font(semibold(13.0)));
                    note(ui, &summary);
                }
                ui.add_space(8.0);
                if ui.button("Manage rules...").clicked() {
                    self.dialog = Some(Dialog::Rules);
                }
            }
            Page::Junk => {
                let s = &mut self.settings;
                heading(ui, "Blocked senders");
                note(ui, "Mail from these goes straight to Junk. An address, or a whole domain (@example.com).");
                list_editor(ui, "blocked", &mut s.blocked, &mut self.new_blocked);
                heading(ui, "Safe senders");
                note(ui, "Never blocked; with that choice on Reading pane, their pictures show.");
                list_editor(ui, "safe", &mut s.safe, &mut self.new_safe);
            }
            Page::Sync => {
                let s = &mut self.settings;
                heading(ui, "Checking for new mail");
                ui.horizontal(|ui| {
                    ui.label("Check every account every");
                    ui.add(egui::DragValue::new(&mut s.check_minutes).range(0..=240));
                    ui.label("minutes");
                });
                note(ui, "0: only when Send / Receive is pressed.");
                if ui.button("Check now").clicked() {
                    for i in 0..self.accounts.len() {
                        self.sync(i);
                    }
                }
            }
            Page::Handling => {
                let s = &mut self.settings;
                heading(ui, "After deleting or moving a message");
                ui.radio_value(&mut s.after_delete, AfterDelete::OpenNext, "Open the next message");
                ui.radio_value(&mut s.after_delete, AfterDelete::OpenPrevious, "Open the previous message");
                ui.radio_value(&mut s.after_delete, AfterDelete::ReturnToList, "Return to the list");
                heading(ui, "Deleting");
                ui.checkbox(&mut s.read_on_delete, "Mark messages as read when they are deleted");
                ui.checkbox(&mut s.empty_trash_on_exit, "Empty Trash when MailCraft closes");
                heading(ui, "Read receipts");
                note(ui, "When a message asks for one:");
                ui.radio_value(&mut s.receipts, Receipts::Ask, "Ask me");
                ui.radio_value(&mut s.receipts, Receipts::Always, "Always send one");
                ui.radio_value(&mut s.receipts, Receipts::Never, "Never send one");
            }
            Page::Templates => {
                note(ui, "Whole messages to start from: in a new message, Templates puts the text in (and the subject, when the message has none).");
                let list = &mut self.settings.templates;
                pick_list(ui, "tpl", list.iter().map(|x| x.name.clone()).collect(), &mut self.set_sel);
                let (add, del) = add_delete(ui, self.set_sel.is_some());
                if add {
                    list.push(crate::settings::Template { name: format!("Template {}", list.len() + 1), ..Default::default() });
                    self.set_sel = Some(list.len() - 1);
                }
                if del && let Some(i) = self.set_sel.take().filter(|i| *i < list.len()) {
                    list.remove(i);
                }
                if let Some(x) = self.set_sel.and_then(|i| list.get_mut(i)) {
                    field(ui, "Name", &mut x.name);
                    field(ui, "Subject", &mut x.subject);
                    ui.label("Text");
                    ui.add(egui::TextEdit::multiline(&mut x.text).desired_width(430.0).desired_rows(8));
                }
            }
            Page::QuickParts => {
                note(ui, "Pieces of text to drop in where the cursor is, from Quick Parts in a new message.");
                let list = &mut self.settings.quick_parts;
                pick_list(ui, "qp", list.iter().map(|x| x.name.clone()).collect(), &mut self.set_sel);
                let (add, del) = add_delete(ui, self.set_sel.is_some());
                if add {
                    list.push(crate::settings::QuickPart { name: format!("Quick part {}", list.len() + 1), ..Default::default() });
                    self.set_sel = Some(list.len() - 1);
                }
                if del && let Some(i) = self.set_sel.take().filter(|i| *i < list.len()) {
                    list.remove(i);
                }
                if let Some(x) = self.set_sel.and_then(|i| list.get_mut(i)) {
                    field(ui, "Name", &mut x.name);
                    ui.label("Text");
                    ui.add(egui::TextEdit::multiline(&mut x.text).desired_width(430.0).desired_rows(6));
                }
            }
            Page::QuickSteps => {
                note(ui, "Several actions under one button: they appear on Home > Quick steps and act on the selected messages.");
                let mine = self.current().map(|a| a.email.clone()).unwrap_or_default();
                let folders = self.folder_choices();
                let cats: Vec<(String, String)> = self.categories.iter().map(|c| (c.key.clone(), c.name.clone())).collect();
                let list = &mut self.settings.quick_steps;
                let shown: Vec<usize> = (0..list.len()).filter(|&i| list.get(i).is_some_and(|q| q.account == mine)).collect();
                let mut sel_pos = self.set_sel.and_then(|i| shown.iter().position(|&j| j == i));
                pick_list(ui, "qs", shown.iter().filter_map(|&i| list.get(i)).map(|q| q.name.clone()).collect(), &mut sel_pos);
                self.set_sel = sel_pos.and_then(|p| shown.get(p).copied());
                let (add, del) = add_delete(ui, self.set_sel.is_some());
                if add {
                    list.push(crate::settings::QuickStep {
                        name: format!("Quick step {}", shown.len() + 1),
                        account: mine.clone(),
                        mark_read: true,
                        ..Default::default()
                    });
                    self.set_sel = Some(list.len() - 1);
                }
                if del && let Some(i) = self.set_sel.take().filter(|i| *i < list.len()) {
                    list.remove(i);
                }
                if let Some(x) = self.set_sel.and_then(|i| list.get_mut(i)) {
                    field(ui, "Name", &mut x.name);
                    ui.label(RichText::new("Do this:").font(semibold(13.0)));
                    ui.checkbox(&mut x.mark_read, "Mark as read");
                    ui.checkbox(&mut x.flag, "Flag");
                    choose(ui, "qs_cat", "Categorise", &mut x.categorise, &cats);
                    ui.checkbox(&mut x.delete, "Delete (to Trash)");
                    if !x.delete {
                        choose(ui, "qs_move", "Move to folder", &mut x.move_to, &folders);
                    }
                }
            }
            Page::SearchFolders => {
                note(ui, "Saved searches across the whole account, shown as folders under Search folders.");
                let mine = self.current().map(|a| a.email.clone()).unwrap_or_default();
                let cats: Vec<(String, String)> = self.categories.iter().map(|c| (c.key.clone(), c.name.clone())).collect();
                let list = &mut self.settings.search_folders;
                let shown: Vec<usize> = (0..list.len()).filter(|&i| list.get(i).is_some_and(|q| q.account == mine)).collect();
                let mut sel_pos = self.set_sel.and_then(|i| shown.iter().position(|&j| j == i));
                pick_list(ui, "sf", shown.iter().filter_map(|&i| list.get(i)).map(|q| q.name.clone()).collect(), &mut sel_pos);
                self.set_sel = sel_pos.and_then(|p| shown.get(p).copied());
                let (add, del) = add_delete(ui, self.set_sel.is_some());
                if add {
                    list.push(crate::settings::SearchFolder {
                        name: "Unread mail".into(),
                        account: mine.clone(),
                        unread_only: true,
                        ..Default::default()
                    });
                    self.set_sel = Some(list.len() - 1);
                }
                if del && let Some(i) = self.set_sel.take().filter(|i| *i < list.len()) {
                    list.remove(i);
                }
                if let Some(x) = self.set_sel.and_then(|i| list.get_mut(i)) {
                    field(ui, "Name", &mut x.name);
                    field(ui, "Words", &mut x.words);
                    ui.checkbox(&mut x.unread_only, "Unread only");
                    ui.checkbox(&mut x.flagged_only, "Flagged only");
                    choose(ui, "sf_cat", "With the category", &mut x.category, &cats);
                }
            }
            Page::Formatting => {
                note(ui, "Messages in the list that match are shown in a colour (all conditions filled in must hold).");
                let list = &mut self.settings.formats;
                pick_list(ui, "cf", list.iter().map(|x| format!("{}{}", if x.on { "" } else { "(off) " }, x.name)).collect(), &mut self.set_sel);
                let (add, del) = add_delete(ui, self.set_sel.is_some());
                if add {
                    list.push(crate::settings::CondFormat { name: format!("Format {}", list.len() + 1), ..Default::default() });
                    self.set_sel = Some(list.len() - 1);
                }
                if del && let Some(i) = self.set_sel.take().filter(|i| *i < list.len()) {
                    list.remove(i);
                }
                if let Some(x) = self.set_sel.and_then(|i| list.get_mut(i)) {
                    field(ui, "Name", &mut x.name);
                    ui.checkbox(&mut x.on, "On");
                    field(ui, "From contains", &mut x.from);
                    field(ui, "Subject contains", &mut x.subject);
                    ui.checkbox(&mut x.unread_only, "Unread messages only");
                    ui.horizontal(|ui| {
                        ui.label("Colour");
                        ui.color_edit_button_srgb(&mut x.colour);
                        ui.checkbox(&mut x.bold, "Bold");
                    });
                }
            }
            Page::Sweep => {
                note(ui, "Standing sweeps tidy a folder after every sync. Start one from a message: Home > Sweep, or right-click > Sweep.");
                let mine = self.current().map(|a| a.email.clone()).unwrap_or_default();
                let mut remove = None;
                for (i, w) in self.settings.sweeps.iter().enumerate().filter(|(_, w)| w.account == mine) {
                    ui.horizontal(|ui| {
                        let what = match (w.keep_latest, w.older_than_days) {
                            (true, _) => "keep only the latest".to_string(),
                            (false, Some(d)) => format!("keep the last {d} days"),
                            _ => "remove all".to_string(),
                        };
                        let then = w.move_to.as_deref().map(|f| format!("move the rest to {f}")).unwrap_or_else(|| "delete the rest".into());
                        ui.label(format!("{} in {}: {what}, {then}", w.sender, w.folder));
                        if ui.small_button("Remove").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    self.settings.sweeps.remove(i);
                }
                if !self.settings.sweeps.iter().any(|w| w.account == mine) {
                    ui.label(RichText::new("No standing sweeps.").color(t.text_dim));
                }
            }
            Page::Actions => {
                note(ui, "Buttons shown on a message in the list while the mouse is over it.");
                use crate::settings::HoverAction as H;
                for (a, label) in
                    [(H::Delete, "Delete"), (H::Archive, "Archive"), (H::Flag, "Flag / unflag"), (H::ReadUnread, "Mark as read / unread")]
                {
                    let mut on = self.settings.hover_actions.contains(&a);
                    if ui.checkbox(&mut on, label).changed() {
                        if on {
                            self.settings.hover_actions.push(a);
                        } else {
                            self.settings.hover_actions.retain(|x| *x != a);
                        }
                    }
                }
            }
        }
    }
}

impl MailApp {
    /// The account being edited, inside its card.
    fn account_form_ui(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let mut test = false;
        let mut test_send = false;
        let mut save = false;
        let mut cancel = false;
        let mut sign_in = false;
        let Some(f) = self.account_form.as_mut() else { return };
        ui.separator();
        let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
            ui.horizontal(|ui| {
                ui.add_sized([150.0, 20.0], egui::Label::new(label));
                add(ui);
            });
        };
        let security = |ui: &mut egui::Ui, cfg: &mut crate::accounts::ServerCfg, tls: u16, starttls: u16| {
            use mailcraft_autoconfig::Security as S;
            let before = cfg.security;
            ui.radio_value(&mut cfg.security, S::Tls, "SSL/TLS");
            ui.radio_value(&mut cfg.security, S::StartTls, "STARTTLS");
            if cfg.security != before && (cfg.port == tls || cfg.port == starttls) {
                cfg.port = if cfg.security == S::Tls { tls } else { starttls };
            }
        };
        ui.label(RichText::new("Incoming mail (IMAP)").font(semibold(12.5)));
        row(ui, "Server", &mut |ui| {
            ui.add_sized([240.0, 22.0], egui::TextEdit::singleline(&mut f.imap.host).id(egui::Id::new("acc_imap")));
            ui.label("Port");
            ui.add(egui::DragValue::new(&mut f.imap.port).range(1..=65535));
        });
        row(ui, "Security", &mut |ui| security(ui, &mut f.imap, 993, 143));
        ui.label(RichText::new("Outgoing mail (SMTP)").font(semibold(12.5)));
        row(ui, "Server", &mut |ui| {
            ui.add_sized([240.0, 22.0], egui::TextEdit::singleline(&mut f.smtp.host).id(egui::Id::new("acc_smtp")).hint_text("none: can't send"));
            ui.label("Port");
            ui.add(egui::DragValue::new(&mut f.smtp.port).range(1..=65535));
        });
        row(ui, "Security", &mut |ui| security(ui, &mut f.smtp, 465, 587));
        row(ui, "User name", &mut |ui| {
            ui.add_sized([240.0, 22.0], egui::TextEdit::singleline(&mut f.username).id(egui::Id::new("acc_user")));
        });
        if f.oauth {
            row(ui, "Sign-in", &mut |ui| {
                ui.label(RichText::new("through the provider's page").color(t.text_dim));
                if ui.button("Sign in again...").clicked() {
                    sign_in = true;
                }
            });
        } else {
            row(ui, "Password", &mut |ui| {
                ui.add_sized(
                    [240.0, 22.0],
                    egui::TextEdit::singleline(&mut f.password).id(egui::Id::new("acc_pw")).password(true).hint_text("unchanged"),
                );
            });
        }
        match &f.result {
            Some(Ok(m)) => {
                ui.colored_label(t.green, m);
            }
            Some(Err(e)) => {
                ui.colored_label(t.red, e);
            }
            None if f.busy => {
                ui.label(RichText::new("Signing in...").color(t.text_dim));
            }
            None => {}
        }
        let problem = f.problem();
        if let Some(p) = problem {
            ui.label(RichText::new(p).font(regular(11.5)).color(t.text_dim));
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(problem.is_none() && !f.busy, egui::Button::new("Test"))
                .on_hover_text("Sign in to the incoming server; nothing changes")
                .clicked()
            {
                test = true;
            }
            if ui
                .add_enabled(problem.is_none() && !f.busy && !f.smtp.host.trim().is_empty(), egui::Button::new("Test sending"))
                .on_hover_text("Sign in to the outgoing server as sending would; nothing is sent")
                .clicked()
            {
                test_send = true;
            }
            if ui.add_enabled(problem.is_none(), egui::Button::new(RichText::new("Save").font(semibold(12.5)))).clicked() {
                save = true;
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
        });
        let Some(f) = self.account_form.as_mut() else { return };
        let Some(acct) = self.accounts.get(f.index).cloned() else { return };
        if test {
            let changed = f.apply(&acct);
            let secret = if f.password.is_empty() { crate::secrets::get(&acct.email).unwrap_or_default() } else { f.password.clone() };
            f.busy = true;
            f.result = None;
            let _ = self.worker.send(crate::worker::Request::TestImap(changed, secret));
        }
        if test_send {
            let changed = f.apply(&acct);
            let secret = if f.password.is_empty() { crate::secrets::get(&acct.email).unwrap_or_default() } else { f.password.clone() };
            f.busy = true;
            f.result = None;
            let _ = self.worker.send(crate::worker::Request::TestSmtpAccount(changed, secret));
        }
        if sign_in {
            let changed = f.apply(&acct);
            self.start_sign_in(changed);
        }
        if save && let Some(f) = self.account_form.take() {
            let changed = f.apply(&acct);
            if !f.password.is_empty() {
                crate::secrets::set(&acct.email, &f.password);
            }
            if let Some(slot) = self.accounts.get_mut(f.index) {
                *slot = changed;
            }
            match crate::accounts::save(&self.accounts) {
                Ok(()) => self.status = format!("{} saved.", acct.email),
                Err(e) => self.status = format!("Could not save the account: {e}"),
            }
            if f.index == self.account {
                self.sync(f.index);
            }
        }
        if cancel {
            self.account_form = None;
        }
    }

    /// Send-only addresses: send from them (their own SMTP server) without their mail, or a
    /// folder list, here. Sent copies go in the account's own Sent.
    fn senders_section(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        ui.label(
            RichText::new("Addresses to send from with their own outgoing server, without their mail or folders here. Pick one in From when writing; a copy goes in the account's Sent.")
                .font(regular(11.5))
                .color(t.text_dim),
        );
        ui.add_space(4.0);
        let mut edit = None;
        let mut remove = None;
        for (i, snd) in self.senders.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(snd.label()).font(semibold(12.5)));
                let used = if snd.account.is_empty() { "any account".to_string() } else { snd.account.clone() };
                let how = snd.oauth.map(|p| format!(" \u{00b7} signed in with {}", p.name())).unwrap_or_default();
                ui.label(
                    RichText::new(format!("{} : {}{how} \u{00b7} for {used}", snd.smtp.host, snd.smtp.port)).font(regular(11.5)).color(t.text_dim),
                );
                if ui.small_button("Change...").clicked() {
                    edit = Some(i);
                }
                if ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = edit
            && let Some(snd) = self.senders.get(i)
        {
            self.sender_form = Some(SenderForm::from(i, snd));
        }
        if let Some(i) = remove.filter(|i| *i < self.senders.len()) {
            self.senders.remove(i);
            if let Err(e) = crate::accounts::save_senders(&self.senders) {
                self.status = format!("Could not save the send-only addresses: {e}");
            }
        }
        if self.sender_form.is_none() {
            if ui.button("Add a send-only address...").clicked() {
                self.sender_form = Some(SenderForm::new());
            }
            return;
        }
        let accounts: Vec<String> = self.accounts.iter().map(|a| a.email.clone()).collect();
        let mut find = false;
        let mut sign_in = false;
        let mut test = false;
        let mut save = false;
        let mut cancel = false;
        let Some(f) = self.sender_form.as_mut() else { return };
        egui::Frame::group(ui.style()).fill(t.panel).stroke(Stroke::new(1.0, t.border)).corner_radius(6.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(if f.editing.is_some() { "Change the send-only address" } else { "New send-only address" }).font(semibold(13.0)));
            let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
                ui.horizontal(|ui| {
                    ui.add_sized([150.0, 20.0], egui::Label::new(label));
                    add(ui);
                });
            };
            row(ui, "Address", &mut |ui| {
                ui.add_sized([260.0, 22.0], egui::TextEdit::singleline(&mut f.email).id(egui::Id::new("snd_email")).hint_text("me@example.com"));
                if ui.add_enabled(!f.busy && f.email.contains('@'), egui::Button::new("Find settings")).clicked() {
                    find = true;
                }
            });
            row(ui, "Your name on it", &mut |ui| {
                ui.add_sized([260.0, 22.0], egui::TextEdit::singleline(&mut f.name).id(egui::Id::new("snd_name")));
            });
            row(ui, "Outgoing server", &mut |ui| {
                ui.add_sized([260.0, 22.0], egui::TextEdit::singleline(&mut f.host).id(egui::Id::new("snd_host")).hint_text("smtp.example.com"));
                ui.label("Port");
                ui.add(egui::DragValue::new(&mut f.port).range(1..=65535));
            });
            row(ui, "Security", &mut |ui| {
                use mailcraft_autoconfig::Security as S;
                let before = f.security;
                ui.radio_value(&mut f.security, S::Tls, "SSL/TLS");
                ui.radio_value(&mut f.security, S::StartTls, "STARTTLS");
                // The usual port goes with each, unless one was typed.
                if f.security != before && matches!(f.port, 465 | 587) {
                    f.port = if f.security == S::Tls { 465 } else { 587 };
                }
            });
            row(ui, "User name", &mut |ui| {
                ui.add_sized(
                    [260.0, 22.0],
                    egui::TextEdit::singleline(&mut f.username).id(egui::Id::new("snd_user")).hint_text("usually the address"),
                );
            });
            let saved = f.editing.is_some();
            // Outlook.com and Gmail addresses: their own sign-in (Microsoft refuses passwords).
            if let Some(p) = crate::accounts::provider_for(&f.email, &f.host) {
                if !f.auth_chosen {
                    f.oauth = Some(p);
                }
                row(ui, "Sign in with", &mut |ui| {
                    if ui.radio_value(&mut f.oauth, Some(p), format!("{} account", p.name())).changed()
                        | ui.radio_value(&mut f.oauth, None, "A password").changed()
                    {
                        f.auth_chosen = true;
                    }
                });
                if p == mailcraft_core::oauth::Provider::Microsoft && f.oauth.is_none() {
                    ui.label(
                        RichText::new("Microsoft no longer accepts passwords for Outlook.com, Hotmail, Live and MSN addresses: sign in with the Microsoft account.")
                            .font(regular(11.5))
                            .color(t.orange),
                    );
                }
            } else {
                f.oauth = None;
            }
            if let Some(p) = f.oauth {
                row(ui, "", &mut |ui| {
                    if ui.add_enabled(!f.busy && f.email.contains('@'), egui::Button::new(format!("Sign in with {}...", p.name()))).clicked() {
                        sign_in = true;
                    }
                    ui.label(RichText::new("opens your browser").font(regular(11.5)).color(t.text_dim));
                });
            } else {
                row(ui, "Password", &mut |ui| {
                    ui.add_sized(
                        [260.0, 22.0],
                        egui::TextEdit::singleline(&mut f.password).id(egui::Id::new("snd_pw")).password(true).hint_text(if saved {
                            "unchanged"
                        } else {
                            "app password, if the provider uses them"
                        }),
                    );
                });
            }
            row(ui, "Offer it for", &mut |ui| {
                let shown = if f.account.is_empty() { "Any account".to_string() } else { f.account.clone() };
                egui::ComboBox::from_id_salt("snd_account").width(260.0).selected_text(shown).show_ui(ui, |ui| {
                    ui.selectable_value(&mut f.account, String::new(), "Any account");
                    for a in &accounts {
                        ui.selectable_value(&mut f.account, a.clone(), a.as_str());
                    }
                });
            });
            match &f.result {
                Some(Ok(m)) => {
                    ui.colored_label(t.green, m);
                }
                Some(Err(e)) => {
                    ui.colored_label(t.red, e);
                }
                None if f.busy => {
                    ui.label(RichText::new("Asking...").color(t.text_dim));
                }
                None => {}
            }
            let problem = f.problem();
            if let Some(p) = problem {
                ui.label(RichText::new(p).font(regular(11.5)).color(t.text_dim));
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(problem.is_none() && !f.busy, egui::Button::new("Test"))
                    .on_hover_text("Sign in to the server; nothing is sent")
                    .clicked()
                {
                    test = true;
                }
                if ui.add_enabled(problem.is_none(), egui::Button::new(RichText::new("Save").font(semibold(12.5)))).clicked() {
                    save = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if find && let Some(f) = self.sender_form.as_mut() {
            f.busy = true;
            f.result = None;
            let _ = self.worker.send(crate::worker::Request::Discover(f.email.trim().to_string()));
        }
        if sign_in && let Some(f) = self.sender_form.as_mut() {
            f.busy = true;
            f.result = None;
            let _ = self.worker.send(crate::worker::Request::SignIn(f.sender().as_account()));
        }
        if test && let Some(f) = self.sender_form.as_mut() {
            let snd = f.sender();
            let password = if f.password.is_empty() { crate::secrets::get(&snd.secret_key()).unwrap_or_default() } else { f.password.clone() };
            f.busy = true;
            f.result = None;
            let via =
                crate::worker::Via { endpoint: snd.endpoint(), username: snd.username.clone(), password, oauth: snd.oauth.map(|_| snd.as_account()) };
            let _ = self.worker.send(crate::worker::Request::TestSmtp(snd.email.clone(), Box::new(via)));
        }
        if save && let Some(f) = self.sender_form.take() {
            let snd = f.sender();
            if !f.password.is_empty() && snd.oauth.is_none() {
                crate::secrets::set(&snd.secret_key(), &f.password);
            }
            match f.editing.filter(|i| *i < self.senders.len()) {
                Some(i) => {
                    if let Some(slot) = self.senders.get_mut(i) {
                        *slot = snd;
                    }
                }
                None => {
                    self.senders.retain(|x| !x.email.eq_ignore_ascii_case(&snd.email));
                    self.senders.push(snd);
                }
            }
            if let Err(e) = crate::accounts::save_senders(&self.senders) {
                self.status = format!("Could not save the send-only addresses: {e}");
            }
        }
        if cancel {
            self.sender_form = None;
        }
    }
}

/// The page a settings tab is on.
fn tab_page(t: &crate::MainTab) -> Page {
    match t {
        crate::MainTab::Settings { page, .. } | crate::MainTab::Accounts { page, .. } => *page,
        crate::MainTab::Mail | crate::MainTab::Calendar | crate::MainTab::Message { .. } => Page::MessageList,
    }
}

fn text_size(ui: &mut egui::Ui, v: &mut TextSize) {
    ui.horizontal(|ui| {
        ui.radio_value(v, TextSize::Small, "Small");
        ui.radio_value(v, TextSize::Medium, "Medium");
        ui.radio_value(v, TextSize::Large, "Large");
    });
}

fn animations(ui: &mut egui::Ui, v: &mut Animations) {
    ui.radio_value(v, Animations::System, "Use the system setting");
    ui.radio_value(v, Animations::On, "Show animations");
    ui.radio_value(v, Animations::Off, "Don't show animations");
}

/// A list to pick one item from.
fn pick_list(ui: &mut egui::Ui, id: &str, names: Vec<String>, sel: &mut Option<usize>) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(430.0);
        if names.is_empty() {
            ui.label(RichText::new("None yet.").color(egui::Color32::GRAY));
        }
        egui::ScrollArea::vertical().id_salt(("pick", id)).max_height(120.0).show(ui, |ui| {
            for (i, n) in names.iter().enumerate() {
                if ui.add(egui::Button::selectable(*sel == Some(i), n.as_str()).min_size(vec2(410.0, 20.0))).clicked() {
                    *sel = Some(i);
                }
            }
        });
    });
}

/// "Add" and "Delete" under a list: which was pressed.
fn add_delete(ui: &mut egui::Ui, have: bool) -> (bool, bool) {
    let mut out = (false, false);
    ui.horizontal(|ui| {
        out.0 = ui.button("Add").clicked();
        out.1 = ui.add_enabled(have, egui::Button::new("Delete")).clicked();
    });
    ui.add_space(6.0);
    out
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.add_sized([110.0, 20.0], egui::Label::new(label));
        ui.add(egui::TextEdit::singleline(value).desired_width(310.0));
    });
}

/// A tick box with a choice beside it, for an optional setting (a folder, a category).
fn choose(ui: &mut egui::Ui, id: &str, label: &str, slot: &mut Option<String>, choices: &[(String, String)]) {
    ui.horizontal(|ui| {
        let mut on = slot.is_some();
        ui.checkbox(&mut on, label);
        if on {
            let mut v = slot.clone().or_else(|| choices.first().map(|(k, _)| k.clone())).unwrap_or_default();
            let shown = choices.iter().find(|(k, _)| *k == v).map(|(_, n)| n.trim().to_string()).unwrap_or_else(|| v.clone());
            egui::ComboBox::from_id_salt(id).width(220.0).selected_text(shown).show_ui(ui, |ui| {
                for (k, n) in choices {
                    ui.selectable_value(&mut v, k.clone(), n.as_str());
                }
            });
            *slot = Some(v);
        } else {
            *slot = None;
        }
    });
}

/// A list of addresses with Remove buttons and a box to add one.
fn list_editor(ui: &mut egui::Ui, id: &str, list: &mut Vec<String>, entry: &mut String) {
    let mut remove = None;
    for (i, e) in list.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(e);
            if ui.small_button("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        list.remove(i);
    }
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(entry)
                .id(egui::Id::new(("set_add", id)))
                .desired_width(260.0)
                .hint_text("someone@example.com or @example.com"),
        );
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.button("Add").clicked() || enter {
            let v = entry.trim().to_string();
            if !v.is_empty() && !list.iter().any(|x| x.eq_ignore_ascii_case(&v)) {
                list.push(v);
            }
            entry.clear();
        }
    });
}
