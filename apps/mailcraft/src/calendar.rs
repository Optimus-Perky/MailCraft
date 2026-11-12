//! The Calendar tab (Betterbird's calendar space): a month, week or day of events from the
//! calendar accounts (the mail server's own calendar, Nextcloud, or any CalDAV server), a small
//! month to move about with, the calendars to show or hide, and an editor for events.
//!
//! Calendar accounts are kept in calendars.json; their passwords in the keyring as
//! "calendar:<id>" (or, for the mail server's calendar, the mail account's own; for Google, the
//! sign-in's refresh token). Events are kept in the mail cache (mailcraft_core::calstore) and
//! synced in the background.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, Duration as Span, NaiveDate, NaiveTime, TimeZone, Timelike, Utc, Weekday};
use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, pos2, vec2};
use mailcraft_core::Store;
use mailcraft_core::caldav::{Auth, Calendar, Client};
use mailcraft_core::calstore::{self, CalRow, Remote, Shown, SyncReport};
use mailcraft_core::ical::{self, Event, Tz, When};
use mailcraft_core::oauth::{self, Provider, Purpose};
use serde::{Deserialize, Serialize};
use wordcraft_ui_egui::theme::{Tokens, regular, semibold};

use crate::MailApp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    /// The calendar on the mail account's own server, signed in to as the mail is.
    MailServer,
    Nextcloud,
    CalDav,
    /// Google Calendar: CalDAV, signed in with Google (the username is the Google address).
    Google,
    /// Outlook.com / Microsoft 365: Microsoft Graph, signed in with Microsoft.
    Microsoft,
}

/// Google's CalDAV.
const GOOGLE_CALDAV: &str = "https://apidata.googleusercontent.com/caldav/v2/";

/// Calendar access tokens in hand, per calendar account, with when they stop working. Mail's are
/// kept apart (a different permission). Never written to disk.
static TOKENS: Mutex<Option<HashMap<String, (String, Instant)>>> = Mutex::new(None);

/// A calendar account: where its calendars are and who to sign in as.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalSource {
    pub id: String,
    pub label: String,
    pub kind: SourceKind,
    /// MailServer: the mail account (address) whose sign-in it uses.
    #[serde(default)]
    pub account: String,
    pub server: String,
    pub username: String,
    /// The calendar home, once found.
    #[serde(default)]
    pub home: String,
    /// Test servers only: allow plain http.
    #[serde(default)]
    pub allow_insecure: bool,
    /// The calendar server emails meeting invitations, replies and cancellations itself (a
    /// Nextcloud set up for it). Off: MailCraft mails them. Google and Microsoft always do their
    /// own. (Servers that say they schedule often do not send mail: Poste.io's does not.)
    #[serde(default)]
    pub server_sends_invites: bool,
}

impl CalSource {
    /// The keyring entry holding its password.
    pub fn secret_name(&self) -> String {
        match self.kind {
            SourceKind::MailServer => self.account.clone(),
            _ => format!("calendar:{}", self.id),
        }
    }

    /// Where finding the calendars starts.
    pub fn start_url(&self) -> String {
        let s = self.server.trim().trim_end_matches('/');
        match self.kind {
            // Nextcloud keeps CalDAV under remote.php/dav (its .well-known redirect needs the
            // web server set up for it; this does not).
            SourceKind::Nextcloud if !s.contains("/remote.php") => {
                let base = if s.contains("://") { s.to_string() } else { format!("https://{s}") };
                format!("{base}/remote.php/dav/")
            }
            // The person's principal: its calendar-home-set says where their calendars are.
            SourceKind::Google => format!("{GOOGLE_CALDAV}{}/user", self.username.trim()),
            SourceKind::Microsoft => mailcraft_core::graph::base(),
            _ => s.to_string(),
        }
    }

    /// The provider it signs in with (through the provider's page, no password).
    pub fn provider(&self) -> Option<Provider> {
        match self.kind {
            SourceKind::Google => Some(Provider::Google),
            SourceKind::Microsoft => Some(Provider::Microsoft),
            _ => None,
        }
    }

    /// Whether new calendars can be made in it (Google's CalDAV does not do MKCALENDAR).
    pub fn can_make_calendars(&self) -> bool {
        self.kind != SourceKind::Google
    }

    /// Whether the server mails this event's meeting messages itself (else MailCraft does).
    fn server_sends(&self, c: &Remote, calendar: &str, e: &Event, before: Option<&Event>) -> bool {
        match self.kind {
            // Google's CalDAV says it schedules and does; Microsoft does for any event with guests.
            SourceKind::Google | SourceKind::Microsoft => c.schedules(calendar, e, before),
            _ => self.server_sends_invites,
        }
    }

    /// What to say when its password (or sign-in) is missing.
    pub fn missing_secret(&self) -> String {
        if let Some(p) = self.provider() {
            format!("Not signed in: edit the calendar account and sign in with {}.", p.name())
        } else {
            "No saved password: edit the calendar account to enter it.".into()
        }
    }

    /// A client signed in as this account. For Google and Microsoft `secret` is the refresh
    /// token: the access token in hand is used, or a new one fetched.
    fn client(&self, secret: &str) -> Result<Remote, String> {
        if self.kind == SourceKind::Microsoft {
            return Ok(Remote::Graph(mailcraft_core::graph::Client::new(&self.access_token(secret)?)));
        }
        let auth = match self.kind {
            SourceKind::Google => Auth::Bearer(self.access_token(secret)?),
            _ => Auth::Basic { user: self.username.clone(), pass: secret.to_string() },
        };
        let mut c = Client::new(auth);
        c.insecure_ok = self.allow_insecure;
        Ok(Remote::CalDav(c))
    }

    fn access_token(&self, refresh: &str) -> Result<String, String> {
        if let Some((t, until)) = TOKENS.lock().ok().and_then(|m| m.as_ref().and_then(|m| m.get(&self.id).cloned()))
            && Instant::now() < until
        {
            return Ok(t);
        }
        let p = self.provider().ok_or("this calendar account signs in with a password")?;
        let client = crate::accounts::oauth_client(p).ok_or_else(|| format!("MailCraft has no {} app registration", p.name()))?;
        // The saved token, not the copy this request carried: a refresh may have rotated it.
        let refresh = crate::secrets::get(&self.secret_name()).unwrap_or_else(|| refresh.to_string());
        match oauth::refresh_for(p, &client, &refresh, Purpose::Calendar) {
            Ok(t) => {
                self.keep(&t, &refresh);
                Ok(t.access_token)
            }
            Err(e) if oauth::is_revoked(&e) => {
                Err(format!("{} no longer accepts the saved sign-in: edit the calendar account and sign in again ({e})", p.name()))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Remember new tokens: the access token in memory, a new refresh token in the keyring.
    fn keep(&self, t: &oauth::Tokens, old_refresh: &str) {
        if let Some(r) = t.refresh_token.as_deref().filter(|r| *r != old_refresh) {
            crate::secrets::set(&self.secret_name(), r);
        }
        let until = Instant::now() + t.expires_in.saturating_sub(Duration::from_secs(120));
        if let Ok(mut m) = TOKENS.lock() {
            m.get_or_insert_with(HashMap::new).insert(self.id.clone(), (t.access_token.clone(), until));
        }
    }

    /// Where the calendars are, found from the start address.
    fn home(&self, c: &Remote) -> Result<String, String> {
        if !self.home.is_empty() {
            return Ok(self.home.clone());
        }
        c.discover(&self.start_url()).map_err(|e| e.to_string())
    }
}

/// The browser sign-in for a calendar account (Google, Microsoft). Runs on a thread of its own: it
/// waits on the person. On success the refresh token is in the keyring; gives the address signed
/// in as.
pub fn sign_in(src: &CalSource) -> Result<String, String> {
    let p = src.provider().ok_or("this calendar account signs in with a password")?;
    let client = crate::accounts::oauth_client(p)
        .ok_or_else(|| format!("MailCraft has no {} app registration yet (Settings > Accounts, Sign in with {0})", p.name()))?;
    let t = oauth::sign_in_for(p, &client, src.username.trim(), Purpose::Calendar, crate::worker::open_browser).map_err(|e| e.to_string())?;
    src.keep(&t, "");
    if let Some(r) = t.refresh_token.as_deref() {
        crate::secrets::set(&src.secret_name(), r);
    }
    Ok(t.email.unwrap_or_else(|| src.username.trim().to_string()))
}

pub fn load_sources() -> Vec<CalSource> {
    crate::accounts::read_settings("calendars.json").unwrap_or_default()
}

fn save_sources(s: &[CalSource]) -> std::io::Result<()> {
    crate::accounts::write_settings("calendars.json", &s)
}

/// A meeting message to go with a save or a delete (iTIP, RFC 5546): an invitation (REQUEST),
/// an answer (REPLY) or a cancellation (CANCEL). It is mailed only when the calendar server
/// does not send it itself.
#[derive(Debug, Clone)]
pub struct Itip {
    pub method: String,
    pub event: Event,
    pub to: Vec<String>,
    /// The address it is from (the organiser, or the one answering).
    pub from: String,
}

/// Calendar work for the background thread.
pub enum CalRequest {
    Test(CalSource, String),
    Sync(CalSource, String),
    Save(CalSource, String, i64, Option<i64>, Box<Event>, Vec<Itip>),
    Delete(CalSource, String, i64, Vec<Itip>),
    /// A new calendar (name, colour) in the account; it is synced after.
    MakeCalendar(CalSource, String, String, Option<String>),
}

pub enum CalEvent {
    /// A calendar account's browser sign-in finished (the address signed in as).
    SignedIn(String, Result<String, String>),
    Tested(String, Result<(String, Vec<Calendar>), String>),
    Synced(String, Result<(String, SyncReport), String>),
    /// Saved or deleted; with the meeting message MailCraft has to mail (the server did not).
    Saved(Result<String, String>, Vec<Itip>),
}

/// Runs on the worker thread (blocking: the calendar servers are spoken to over plain HTTP calls).
pub fn run(store: Option<&Store>, req: CalRequest) -> CalEvent {
    let no_store = || "the local cache could not be opened".to_string();
    match req {
        CalRequest::Test(src, secret) => {
            let r = (|| {
                let c = src.client(&secret)?;
                let src = CalSource { home: String::new(), ..src.clone() };
                let home = src.home(&c)?;
                let cals = c.calendars(&home).map_err(|e| e.to_string())?;
                Ok((home, cals))
            })();
            CalEvent::Tested(src.id, r)
        }
        CalRequest::Sync(src, secret) => {
            let r = (|| {
                let store = store.ok_or_else(no_store)?;
                let c = src.client(&secret)?;
                let home = src.home(&c)?;
                let rep = calstore::sync(store, &c, &src.id, &home).map_err(|e| e.to_string())?;
                Ok((home, rep))
            })();
            CalEvent::Synced(src.id, r)
        }
        CalRequest::Save(src, secret, cal, row, e, itips) => {
            let r = (|| {
                let s = store.ok_or_else(no_store)?;
                let c = src.client(&secret)?;
                // The event as it was, for judging who sends what (it is replaced by the save).
                let before =
                    row.and_then(|r| s.event_row(r).ok().flatten()).and_then(|r| ical::parse(&r.ics).into_iter().find(|e| e.recurrence_id.is_none()));
                calstore::save(s, &c, cal, row, &e).map_err(|e| e.to_string())?;
                // Mailed by MailCraft only when the server does not send them itself.
                let url = s.calendar(cal).ok().flatten().map(|c| c.url).unwrap_or_default();
                if itips.is_empty() || src.server_sends(&c, &url, &e, before.as_ref()) { Ok(vec![]) } else { Ok(itips) }
            })();
            match r {
                Ok(itips) => CalEvent::Saved(Ok("Event saved.".to_string()), itips),
                Err(e) => CalEvent::Saved(Err(e), vec![]),
            }
        }
        CalRequest::MakeCalendar(src, secret, name, colour) => {
            let r = (|| {
                let store = store.ok_or_else(no_store)?;
                let c = src.client(&secret)?;
                let home = src.home(&c)?;
                c.make_calendar(&home, &name, colour.as_deref()).map_err(|e| e.to_string())?;
                let rep = calstore::sync(store, &c, &src.id, &home).map_err(|e| e.to_string())?;
                Ok((home, rep))
            })();
            CalEvent::Synced(src.id, r)
        }
        CalRequest::Delete(src, secret, row, itips) => {
            let r = (|| {
                let s = store.ok_or_else(no_store)?;
                let c = src.client(&secret)?;
                // Whether the server schedules is judged by the stored event (before it goes).
                let stored = s.event_row(row).ok().flatten();
                let gone = stored.as_ref().and_then(|r| ical::parse(&r.ics).into_iter().find(|e| e.recurrence_id.is_none()));
                let url = stored.as_ref().and_then(|r| s.calendar(r.calendar_id).ok().flatten()).map(|c| c.url).unwrap_or_default();
                let server_sends = itips.is_empty() || gone.as_ref().is_some_and(|e| src.server_sends(&c, &url, e, None));
                calstore::delete(s, &c, row).map_err(|e| e.to_string())?;
                Ok(if server_sends { vec![] } else { itips })
            })();
            match r {
                Ok(itips) => CalEvent::Saved(Ok("Event deleted.".to_string()), itips),
                Err(e) => CalEvent::Saved(Err(e), vec![]),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Day,
    Week,
    Month,
}

/// How an event repeats, as the editor offers it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Repeat {
    None,
    Daily,
    Weekdays,
    Weekly,
    Monthly,
    Yearly,
    /// A rule made elsewhere, kept as it is.
    Custom(String),
}

impl Repeat {
    fn of(rule: Option<&str>) -> Repeat {
        match rule.map(|r| r.trim().to_ascii_uppercase()) {
            None => Repeat::None,
            Some(r) => match r.as_str() {
                "FREQ=DAILY" => Repeat::Daily,
                "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR" => Repeat::Weekdays,
                "FREQ=WEEKLY" => Repeat::Weekly,
                "FREQ=MONTHLY" => Repeat::Monthly,
                "FREQ=YEARLY" => Repeat::Yearly,
                _ => Repeat::Custom(rule.unwrap_or_default().to_string()),
            },
        }
    }

    fn rule(&self) -> Option<String> {
        Some(
            match self {
                Repeat::None => return None,
                Repeat::Daily => "FREQ=DAILY",
                Repeat::Weekdays => "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR",
                Repeat::Weekly => "FREQ=WEEKLY",
                Repeat::Monthly => "FREQ=MONTHLY",
                Repeat::Yearly => "FREQ=YEARLY",
                Repeat::Custom(r) => r,
            }
            .to_string(),
        )
    }

    fn label(&self, day: NaiveDate) -> String {
        match self {
            Repeat::None => "Does not repeat".into(),
            Repeat::Daily => "Every day".into(),
            Repeat::Weekdays => "Every weekday (Monday to Friday)".into(),
            Repeat::Weekly => format!("Every week on {}", day.format("%A")),
            Repeat::Monthly => format!("Every month on the {}", ordinal(day.day())),
            Repeat::Yearly => format!("Every year on {}", day.format("%-d %B")),
            Repeat::Custom(r) => format!("Custom ({r})"),
        }
    }
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// Reminder choices: minutes before (None: no reminder).
const REMINDERS: &[(Option<i64>, &str)] = &[
    (None, "None"),
    (Some(0), "At the start"),
    (Some(5), "5 minutes before"),
    (Some(10), "10 minutes before"),
    (Some(15), "15 minutes before"),
    (Some(30), "30 minutes before"),
    (Some(60), "1 hour before"),
    (Some(120), "2 hours before"),
    (Some(1440), "1 day before"),
    (Some(10080), "1 week before"),
];

fn reminder_label(m: i64) -> String {
    REMINDERS.iter().find(|(v, _)| *v == Some(m)).map(|(_, l)| l.to_string()).unwrap_or_else(|| match m {
        m if m % 1440 == 0 => format!("{} days before", m / 1440),
        m if m % 60 == 0 => format!("{} hours before", m / 60),
        m => format!("{m} minutes before"),
    })
}

/// The event editor (New event, or Edit).
pub struct EventForm {
    /// The stored event being changed, and the calendar it is in.
    row: Option<i64>,
    from_calendar: Option<i64>,
    calendar: i64,
    /// The event as it was (kept: what the editor does not show survives).
    base: Event,
    /// The occurrence opened, for a repeating event (its start): moving it moves the series.
    instance: Option<DateTime<Utc>>,
    title: String,
    location: String,
    notes: String,
    all_day: bool,
    start_date: NaiveDate,
    start_time: NaiveTime,
    end_date: NaiveDate,
    end_time: NaiveTime,
    repeat: Repeat,
    reminder: Option<i64>,
    /// People invited (addresses, comma separated), when the meeting is mine.
    invite: String,
    /// Someone else's meeting: who invited me (then the guest list is theirs to change).
    invited_by: Option<String>,
    picker: Option<(bool, NaiveDate)>,
    busy: bool,
    error: Option<String>,
}

/// Adding or changing a calendar account (Account settings > Calendars).
pub struct SourceForm {
    /// The account being changed (None: a new one).
    editing: Option<String>,
    kind: SourceKind,
    account: String,
    label: String,
    server: String,
    username: String,
    password: String,
    allow_insecure: bool,
    server_sends_invites: bool,
    id: String,
    busy: bool,
    result: Option<Result<String, String>>,
    found: Vec<Calendar>,
    home: String,
    /// Waiting on the browser sign-in (Google).
    signing_in: bool,
}

pub struct CalState {
    pub sources: Vec<CalSource>,
    pub mode: Mode,
    /// The day in view (the month or week around it).
    pub day: NaiveDate,
    /// The month the small calendar shows (its first day).
    pub mini: NaiveDate,
    cals: Vec<CalRow>,
    cache: Vec<Shown>,
    cache_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
    pub dirty: bool,
    /// An event's details shown after a click, and where.
    popup: Option<(Shown, Pos2, bool)>,
    editor: Option<EventForm>,
    pub form: Option<SourceForm>,
    syncing: HashSet<String>,
    status: HashMap<String, Result<String, String>>,
    last_sync: Option<Instant>,
    /// Reminders shown: (event, instance start, minutes before).
    fired: HashSet<(i64, i64, i64)>,
    upcoming: Vec<Shown>,
    upcoming_at: Option<Instant>,
    /// The week and day views start scrolled to the morning, once.
    scrolled: bool,
    /// A calendar being made: (account id, name, colour).
    pub new_cal: Option<(String, String, usize)>,
}

impl CalState {
    /// The first sync, a few seconds after MailCraft starts (when there is anything to sync).
    pub fn last_sync_due(&self, started: Instant) -> bool {
        self.last_sync.is_none() && !self.sources.is_empty() && started.elapsed() > Duration::from_secs(5)
    }

    pub fn new() -> CalState {
        let today = chrono::Local::now().date_naive();
        CalState {
            sources: load_sources(),
            mode: Mode::Month,
            day: today,
            mini: first_of_month(today),
            cals: vec![],
            cache: vec![],
            cache_range: None,
            dirty: true,
            popup: None,
            editor: None,
            form: None,
            syncing: HashSet::new(),
            status: HashMap::new(),
            last_sync: None,
            fired: HashSet::new(),
            upcoming: vec![],
            upcoming_at: None,
            scrolled: false,
            new_cal: None,
        }
    }
}

fn first_of_month(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap_or(d)
}

fn add_months(d: NaiveDate, n: i32) -> NaiveDate {
    let m0 = d.year() * 12 + d.month0() as i32 + n;
    let (y, m) = (m0.div_euclid(12), m0.rem_euclid(12) as u32 + 1);
    let mut day = d.day();
    loop {
        if let Some(x) = NaiveDate::from_ymd_opt(y, m, day) {
            return x;
        }
        if day <= 28 {
            return d;
        }
        day -= 1;
    }
}

/// The Monday on or before.
fn week_start(d: NaiveDate) -> NaiveDate {
    d - Span::days(d.weekday().num_days_from_monday() as i64)
}

/// Midnight at the start of a local day, as a moment.
fn day_start(d: NaiveDate, tz: Tz) -> DateTime<Utc> {
    When::Date(d).utc(tz)
}

/// Calendar colours: offered for a new calendar, and given to one the server has no colour for.
const PALETTE: [(u8, u8, u8); 6] =
    [(0x2D, 0x4A, 0x8A), (0x2E, 0x7D, 0x32), (0xC6, 0x28, 0x28), (0x8E, 0x24, 0xAA), (0xEF, 0x6C, 0x00), (0x00, 0x83, 0x8F)];

fn colour_of(c: Option<&CalRow>) -> Color32 {
    let palette = PALETTE;
    let parsed = c.and_then(|c| c.colour.as_deref()).and_then(|h| {
        let h = h.trim_start_matches('#');
        let v = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
        Some(Color32::from_rgb(v(0)?, v(2)?, v(4)?))
    });
    parsed.unwrap_or_else(|| {
        let (r, g, b) = palette[c.map(|c| c.id.unsigned_abs() as usize).unwrap_or(0) % palette.len()];
        Color32::from_rgb(r, g, b)
    })
}

/// Text that reads on a fill.
fn ink_on(c: Color32) -> Color32 {
    let l = (c.r() as u32 * 299 + c.g() as u32 * 587 + c.b() as u32 * 114) / 1000;
    if l > 150 { Color32::BLACK } else { Color32::WHITE }
}

fn hm(t: DateTime<Utc>, tz: Tz) -> String {
    t.with_timezone(&tz).format("%H:%M").to_string()
}

/// "Friday 9 October 2026, 10:00 to 11:00", "Saturday 24 October 2026 (all day)"...
fn when_text(s: &Shown, tz: Tz) -> String {
    let (a, b) = (s.at.start.with_timezone(&tz), s.at.end.with_timezone(&tz));
    if s.event.start.is_date() {
        let last = (s.at.end - Span::seconds(1)).with_timezone(&tz).date_naive();
        if last <= a.date_naive() {
            format!("{} (all day)", a.format("%A %-d %B %Y"))
        } else {
            format!("{} to {} (all day)", a.format("%A %-d %B"), last.format("%A %-d %B %Y"))
        }
    } else if a.date_naive() == b.date_naive() || b == a {
        format!("{}, {} to {}", a.format("%A %-d %B %Y"), a.format("%H:%M"), b.format("%H:%M"))
    } else {
        format!("{} {} to {} {}", a.format("%a %-d %b %Y"), a.format("%H:%M"), b.format("%a %-d %b %Y"), b.format("%H:%M"))
    }
}

const HOUR: f32 = 44.0;
const GUTTER: f32 = 56.0;

impl MailApp {
    fn local_tz(&self) -> Tz {
        ical::local_zone()
    }

    /// Open the Calendar tab, or go to it.
    pub(crate) fn open_calendar(&mut self) {
        match self.tabs.iter().position(|t| matches!(t, crate::MainTab::Calendar)) {
            Some(i) => self.active_tab = i,
            None => {
                self.tabs.push(crate::MainTab::Calendar);
                self.active_tab = self.tabs.len() - 1;
            }
        }
        self.cal.dirty = true;
        if self.cal.last_sync.is_none_or(|t| t.elapsed() > Duration::from_secs(120)) {
            self.calendar_sync_all();
        }
    }

    pub(crate) fn calendar_active(&self) -> bool {
        matches!(self.tabs.get(self.active_tab), Some(crate::MainTab::Calendar))
    }

    fn cal_secret(&self, src: &CalSource) -> Option<String> {
        crate::secrets::get(&src.secret_name())
    }

    /// Sync every calendar account whose password is at hand.
    pub(crate) fn calendar_sync_all(&mut self) {
        self.cal.last_sync = Some(Instant::now());
        for src in self.cal.sources.clone() {
            self.calendar_sync(&src);
        }
    }

    fn calendar_sync(&mut self, src: &CalSource) {
        if self.cal.syncing.contains(&src.id) {
            return;
        }
        match self.cal_secret(src) {
            Some(pw) => {
                self.cal.syncing.insert(src.id.clone());
                let _ = self.worker.send(crate::worker::Request::Cal(CalRequest::Sync(src.clone(), pw)));
            }
            None => {
                self.cal.status.insert(src.id.clone(), Err(src.missing_secret()));
            }
        }
    }

    /// A reply from the background thread.
    pub(crate) fn calendar_event(&mut self, ev: CalEvent) {
        match ev {
            CalEvent::SignedIn(id, r) => {
                if let Some(f) = self.cal.form.as_mut().filter(|f| f.id == id) {
                    f.signing_in = false;
                    match r {
                        Ok(email) => {
                            f.username = email;
                            f.busy = true;
                            f.result = Some(Ok("Signed in. Looking for your calendars...".into()));
                            // Find the calendars straight away.
                            let src = CalSource {
                                id: f.id.clone(),
                                label: f.label.clone(),
                                kind: f.kind,
                                account: String::new(),
                                server: f.server.clone(),
                                username: f.username.clone(),
                                home: String::new(),
                                allow_insecure: false,
                                server_sends_invites: false,
                            };
                            match crate::secrets::get(&src.secret_name()) {
                                Some(token) => {
                                    let _ = self.worker.send(crate::worker::Request::Cal(CalRequest::Test(src, token)));
                                }
                                None => {
                                    f.busy = false;
                                    f.result = Some(Err("Signed in, but the sign-in could not be saved in the keyring.".into()));
                                }
                            }
                        }
                        Err(e) => f.result = Some(Err(e)),
                    }
                }
            }
            CalEvent::Tested(id, r) => {
                if let Some(f) = self.cal.form.as_mut().filter(|f| f.id == id) {
                    f.busy = false;
                    match r {
                        Ok((home, cals)) => {
                            f.result = Some(Ok(match cals.len() {
                                0 => "Signed in, but there are no calendars there yet.".to_string(),
                                1 => "Found 1 calendar.".to_string(),
                                n => format!("Found {n} calendars."),
                            }));
                            f.found = cals;
                            f.home = home;
                        }
                        Err(e) => {
                            f.result = Some(Err(e));
                            f.found.clear();
                        }
                    }
                }
            }
            CalEvent::Synced(id, r) => {
                self.cal.syncing.remove(&id);
                match r {
                    Ok((home, rep)) => {
                        if let Some(s) = self.cal.sources.iter_mut().find(|s| s.id == id && s.home != home) {
                            s.home = home;
                            let _ = save_sources(&self.cal.sources);
                        }
                        let when = chrono::Local::now().format("%H:%M");
                        let n = match rep.calendars {
                            1 => "1 calendar".to_string(),
                            n => format!("{n} calendars"),
                        };
                        self.cal.status.insert(id, Ok(format!("{n}, synced at {when}")));
                    }
                    Err(e) => {
                        self.cal.status.insert(id, Err(e));
                    }
                }
                self.cal.dirty = true;
                self.cal.upcoming_at = None;
            }
            CalEvent::Saved(r, itips) => {
                for it in &itips {
                    self.send_itip(it);
                }
                match r {
                    Ok(msg) => {
                        if self.cal.editor.as_ref().is_some_and(|f| f.busy) {
                            self.cal.editor = None;
                        }
                        self.status = msg;
                    }
                    Err(e) => match self.cal.editor.as_mut().filter(|f| f.busy) {
                        Some(f) => {
                            f.busy = false;
                            f.error = Some(e);
                        }
                        None => self.notice(&format!("Calendar: {e}"), true),
                    },
                }
                self.cal.dirty = true;
                self.cal.upcoming_at = None;
            }
        }
    }

    fn source_of(&self, calendar_id: i64) -> Option<CalSource> {
        // The store, not the view's list: an invitation can be answered before the Calendar opens.
        let source = match self.cal.cals.iter().find(|c| c.id == calendar_id) {
            Some(c) => c.source.clone(),
            None => self.store.as_ref()?.calendar(calendar_id).ok()??.source,
        };
        self.cal.sources.iter().find(|s| s.id == source).cloned()
    }

    /// The address I am on a calendar: its own sign-in for Google and Microsoft, the mail
    /// account's for the mail server's calendar, else the mail account in use.
    pub(crate) fn my_address(&self, calendar_id: i64) -> String {
        match self.source_of(calendar_id) {
            Some(src) if src.kind == SourceKind::MailServer && !src.account.is_empty() => src.account,
            Some(src) if src.provider().is_some() && src.username.contains('@') => src.username,
            _ => self.current().map(|a| a.email.clone()).unwrap_or_default(),
        }
    }

    /// Whether an address is mine (any mail account, or this calendar's own address).
    pub(crate) fn is_me(&self, address: &str, calendar_id: i64) -> bool {
        address.eq_ignore_ascii_case(&self.my_address(calendar_id)) || self.accounts.iter().any(|a| a.email.eq_ignore_ascii_case(address))
    }

    /// The meeting message for an event of mine with people invited (None otherwise).
    fn meeting_message(&self, method: &str, calendar_id: i64, e: &Event) -> Option<Itip> {
        let org = e.organizer.as_ref()?;
        if e.attendees.is_empty() || !self.is_me(&org.email, calendar_id) {
            return None;
        }
        let to: Vec<String> = e.attendees.iter().map(|a| a.email.clone()).filter(|a| !a.eq_ignore_ascii_case(&org.email)).collect();
        (!to.is_empty()).then(|| Itip { method: method.into(), event: e.clone(), to, from: org.email.clone() })
    }

    /// A cancellation for the guests of my meeting who are no longer on its list (RFC 5546: a
    /// CANCEL naming just them).
    fn removed_guests_message(&self, calendar_id: i64, before: &Event, after: &Event) -> Option<Itip> {
        let org = before.organizer.as_ref()?;
        if !self.is_me(&org.email, calendar_id) {
            return None;
        }
        let gone: Vec<ical::Attendee> = before
            .attendees
            .iter()
            .filter(|a| !a.email.eq_ignore_ascii_case(&org.email))
            .filter(|a| !after.attendees.iter().any(|b| b.email.eq_ignore_ascii_case(&a.email)))
            .cloned()
            .collect();
        if gone.is_empty() {
            return None;
        }
        let event = Event { attendees: gone.clone(), organizer: Some(org.clone()), status: "CANCELLED".into(), ..after.clone() };
        Some(Itip { method: "CANCEL".into(), event, to: gone.iter().map(|a| a.email.clone()).collect(), from: org.email.clone() })
    }

    /// Mails a meeting message from the mail account it belongs to (or the one in use).
    pub(crate) fn send_itip(&mut self, it: &Itip) {
        let Some(acct) = self.accounts.iter().find(|a| a.email.eq_ignore_ascii_case(&it.from)).or(self.current()).cloned() else {
            self.notice("No mail account to send the meeting message from.", true);
            return;
        };
        let display = self.settings.name_for(&acct.email).filter(|n| !n.trim().is_empty());
        let who = display.clone().unwrap_or_else(|| acct.email.clone());
        let e = &it.event;
        let (subject, text) = match it.method.as_str() {
            "CANCEL" => (format!("Cancelled: {}", e.summary), format!("{who} has cancelled this meeting.")),
            "REPLY" => {
                let (word, line) = match e.attendees.first().map(|a| a.partstat.as_str()) {
                    Some("ACCEPTED") => ("Accepted", "has accepted"),
                    Some("TENTATIVE") => ("Tentative", "has tentatively accepted"),
                    _ => ("Declined", "has declined"),
                };
                (format!("{word}: {}", e.summary), format!("{who} {line} this invitation."))
            }
            _ if e.sequence > 0 => (format!("Updated invitation: {}", e.summary), format!("{who} has changed this meeting.")),
            _ => (format!("Invitation: {}", e.summary), format!("{who} has invited you to this meeting.")),
        };
        let mut text = format!("{text}\n\n{}\n{}", e.summary, crate::invite::when(e));
        if !e.location.is_empty() {
            text.push_str(&format!("\nWhere: {}", e.location));
        }
        if it.method == "REQUEST" && !e.description.is_empty() {
            text.push_str(&format!("\n\n{}", e.description));
        }
        let out = mailcraft_core::send::Outgoing {
            from: acct.email.clone(),
            from_name: display,
            to: it.to.clone(),
            subject,
            text,
            reply_to: Some(acct.reply_to.clone()).filter(|r| !r.trim().is_empty()),
            organisation: Some(acct.organisation.clone()).filter(|o| !o.trim().is_empty()),
            calendar: Some((it.method.clone(), ical::to_itip(&it.method, e))),
            ..Default::default()
        };
        match crate::secrets::get(&acct.email) {
            Some(pw) => {
                let _ = self.worker.send(crate::worker::Request::Send(acct, pw, Box::new(out), None, None, None));
            }
            None => self.notice("The meeting message was not sent: no saved password for the mail account.", true),
        }
    }

    /// The kind of account a stored calendar belongs to.
    pub(crate) fn source_kind_of(&self, calendar_id: i64) -> Option<SourceKind> {
        let cal = self.store.as_ref()?.calendar(calendar_id).ok()??;
        self.cal.sources.iter().find(|s| s.id == cal.source).map(|s| s.kind)
    }

    pub(crate) fn cal_send(&mut self, calendar_id: i64, make: impl FnOnce(CalSource, String) -> CalRequest) -> Result<(), String> {
        let src = self.source_of(calendar_id).ok_or("that calendar's account is no longer set up")?;
        let pw = self.cal_secret(&src).ok_or("no saved password for that calendar account")?;
        let _ = self.worker.send(crate::worker::Request::Cal(make(src, pw)));
        Ok(())
    }

    /// The range the view shows.
    fn view_range(&self) -> (NaiveDate, NaiveDate) {
        let d = self.cal.day;
        match self.cal.mode {
            Mode::Day => (d, d + Span::days(1)),
            Mode::Week => (week_start(d), week_start(d) + Span::days(7)),
            Mode::Month => {
                let first = week_start(first_of_month(d));
                (first, first + Span::days(42))
            }
        }
    }

    fn reload(&mut self) {
        let tz = self.local_tz();
        let (a, b) = self.view_range();
        let (from, to) = (day_start(a, tz), day_start(b, tz));
        if !self.cal.dirty && self.cal.cache_range == Some((from, to)) {
            return;
        }
        self.cal.dirty = false;
        self.cal.cache_range = Some((from, to));
        if let Some(store) = &self.store {
            let known: Vec<&str> = self.cal.sources.iter().map(|s| s.id.as_str()).collect();
            self.cal.cals = store.calendars().unwrap_or_default().into_iter().filter(|c| known.contains(&c.source.as_str())).collect();
            let shown_ids: Vec<i64> = self.cal.cals.iter().filter(|c| c.shown).map(|c| c.id).collect();
            self.cal.cache =
                store.shown_between(from, to, tz).unwrap_or_default().into_iter().filter(|s| shown_ids.contains(&s.calendar_id)).collect();
        }
    }

    /// Reminders: a card for each event whose reminder time has come (checked every few seconds,
    /// the next week's events read again every minute or after a change).
    pub(crate) fn calendar_timers(&mut self, ctx: &egui::Context) {
        if self.cal.sources.is_empty() {
            return;
        }
        let tz = self.local_tz();
        let now = Utc::now();
        if self.cal.upcoming_at.is_none_or(|t| t.elapsed() > Duration::from_secs(60))
            && let Some(store) = &self.store
        {
            self.cal.upcoming_at = Some(Instant::now());
            let shown: Vec<i64> = store.calendars().unwrap_or_default().iter().filter(|c| c.shown).map(|c| c.id).collect();
            self.cal.upcoming = store
                .shown_between(now - Span::hours(1), now + Span::days(8), tz)
                .unwrap_or_default()
                .into_iter()
                .filter(|s| !s.event.alarms.is_empty() && shown.contains(&s.calendar_id))
                .collect();
        }
        // Only reminders due since shortly before MailCraft started (not every one missed).
        let since = now - Span::from_std(self.started.elapsed()).unwrap_or_default() - Span::minutes(10);
        let mut due: Option<Shown> = None;
        for s in &self.cal.upcoming {
            for m in &s.event.alarms {
                let at = s.at.start - Span::minutes(*m);
                let key = (s.row, s.at.start.timestamp(), *m);
                if at <= now && at >= since && now < s.at.end.max(s.at.start + Span::minutes(5)) && !self.cal.fired.contains(&key) {
                    self.cal.fired.insert(key);
                    due = Some(s.clone());
                }
            }
        }
        if let Some(s) = due
            && self.settings.notifications
        {
            let mut lines = vec![when_text(&s, tz)];
            if !s.event.location.is_empty() {
                lines.push(s.event.location.clone());
            }
            let starts = s.at.start - now;
            let heading = if s.event.start.is_date() || starts <= Span::zero() {
                "Reminder \u{00b7} now".to_string()
            } else if starts < Span::hours(1) {
                format!("Reminder \u{00b7} in {} minutes", (starts.num_seconds() + 59) / 60)
            } else {
                format!("Reminder \u{00b7} at {}", hm(s.at.start, tz))
            };
            self.card = Some(crate::Card {
                heading,
                title: if s.event.summary.is_empty() { "(no title)".into() } else { s.event.summary.clone() },
                lines,
                open: None,
                day: Some(s.at.start.with_timezone(&tz).date_naive()),
                born: Instant::now(),
            });
            self.alert = true;
        }
        ctx.request_repaint_after(Duration::from_secs(15));
    }

    /// Go to a day in the calendar (a reminder card was clicked).
    pub(crate) fn calendar_go_to(&mut self, day: NaiveDate) {
        self.open_calendar();
        self.cal.day = day;
        self.cal.mini = first_of_month(day);
        if self.cal.mode == Mode::Month {
            self.cal.mode = Mode::Day;
        }
        self.cal.scrolled = false;
    }

    /// The Calendar tab, filling the window below the tabs.
    pub(crate) fn calendar_view(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        self.reload();
        let full = ui.max_rect();
        let left_w = 236.0;
        let side = Rect::from_min_max(full.min, pos2(full.left() + left_w, full.bottom()));
        let main = Rect::from_min_max(pos2(side.right() + 1.0, full.top()), full.max);
        ui.painter().rect_filled(full, 0.0, t.ribbon);
        ui.painter().line_segment([pos2(side.right(), full.top()), pos2(side.right(), full.bottom())], Stroke::new(1.0, t.border));
        ui.scope_builder(egui::UiBuilder::new().max_rect(side.shrink2(vec2(12.0, 12.0))), |ui| self.calendar_side(ui, t));
        ui.scope_builder(egui::UiBuilder::new().max_rect(main), |ui| self.calendar_main(ui, t));
        let ctx = ui.ctx().clone();
        self.event_popup(&ctx, t);
        self.event_editor(&ctx, t);
    }

    fn new_event_at(&mut self, start: NaiveDate, time: Option<NaiveTime>) {
        let writable: Vec<&CalRow> = self.cal.cals.iter().filter(|c| !c.read_only).collect();
        let Some(cal) = writable.iter().find(|c| c.shown).or(writable.first()).map(|c| c.id) else {
            self.notice("Add a calendar account first (Account settings > Calendars).", true);
            return;
        };
        let st = time.unwrap_or(NaiveTime::from_hms_opt(9, 0, 0).unwrap_or(NaiveTime::MIN));
        let end = st + Span::hours(1);
        let (end_date, end_time) = if end < st { (start + Span::days(1), end) } else { (start, end) };
        self.cal.popup = None;
        self.cal.editor = Some(EventForm {
            row: None,
            from_calendar: None,
            calendar: cal,
            base: Event::default(),
            instance: None,
            title: String::new(),
            location: String::new(),
            notes: String::new(),
            all_day: false,
            start_date: start,
            start_time: st,
            end_date,
            end_time,
            repeat: Repeat::None,
            reminder: Some(15),
            invite: String::new(),
            invited_by: None,
            picker: None,
            busy: false,
            error: None,
        });
    }

    fn edit_event(&mut self, s: &Shown) {
        let tz = self.local_tz();
        let Some(store) = &self.store else { return };
        let Ok(Some(row)) = store.event_row(s.row) else { return };
        // A repeating event is changed as a series: the master, from the stored text.
        let evs = ical::parse(&row.ics);
        let base = if s.event.recurrence_id.is_some() {
            s.event.clone()
        } else {
            evs.into_iter().find(|e| e.recurrence_id.is_none()).unwrap_or_else(|| s.event.clone())
        };
        let (a, b) = (s.at.start.with_timezone(&tz), s.at.end.with_timezone(&tz));
        let all_day = s.event.start.is_date();
        let end_date = if all_day { (b - Span::days(1)).date_naive().max(a.date_naive()) } else { b.date_naive() };
        self.cal.popup = None;
        self.cal.editor = Some(EventForm {
            row: Some(s.row),
            from_calendar: Some(s.calendar_id),
            calendar: s.calendar_id,
            instance: if base.rrule.is_some() { s.at.instance } else { None },
            title: base.summary.clone(),
            location: base.location.clone(),
            notes: base.description.clone(),
            all_day,
            start_date: a.date_naive(),
            start_time: a.time(),
            end_date,
            end_time: b.time(),
            repeat: Repeat::of(base.rrule.as_deref()),
            reminder: base.alarms.first().copied(),
            invite: base
                .attendees
                .iter()
                .filter(|a| !base.organizer.as_ref().is_some_and(|o| o.email.eq_ignore_ascii_case(&a.email)))
                .map(|a| a.email.clone())
                .collect::<Vec<_>>()
                .join(", "),
            invited_by: base
                .organizer
                .as_ref()
                .filter(|o| !self.is_me(&o.email, s.calendar_id))
                .map(|o| if o.name.is_empty() { o.email.clone() } else { format!("{} <{}>", o.name, o.email) }),
            base,
            picker: None,
            busy: false,
            error: None,
        });
    }

    /// Delete an event: the whole of it, or (a repeating one) just the occurrence shown.
    fn delete_event(&mut self, s: &Shown, only_this: bool) {
        self.cal.popup = None;
        let r = if only_this && let Some(inst) = s.at.instance {
            let tz = self.local_tz();
            let Some(store) = &self.store else { return };
            let Ok(Some(row)) = store.event_row(s.row) else { return };
            let Some(mut master) = ical::parse(&row.ics).into_iter().find(|e| e.recurrence_id.is_none()) else { return };
            master.exdates.push(if master.start.is_date() { When::Date(inst.with_timezone(&tz).date_naive()) } else { When::Time(inst) });
            master.sequence += 1;
            let rowid = s.row;
            let itips = self.meeting_message("REQUEST", s.calendar_id, &master).into_iter().collect();
            self.cal_send(s.calendar_id, |src, pw| CalRequest::Save(src, pw, s.calendar_id, Some(rowid), Box::new(master), itips))
        } else {
            let rowid = s.row;
            // My meeting, with people invited: they are told it is off.
            let itips = self
                .store
                .as_ref()
                .and_then(|st| st.event_row(rowid).ok().flatten())
                .and_then(|r| {
                    let mut e = ical::parse(&r.ics).into_iter().find(|e| e.recurrence_id.is_none())?;
                    e.status = "CANCELLED".into();
                    e.sequence += 1;
                    self.meeting_message("CANCEL", s.calendar_id, &e)
                })
                .into_iter()
                .collect();
            self.cal_send(s.calendar_id, |src, pw| CalRequest::Delete(src, pw, rowid, itips))
        };
        if let Err(e) = r {
            self.notice(&format!("Could not delete the event: {e}"), true);
        }
    }

    fn calendar_side(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
        let new =
            egui::Button::new(RichText::new("+  New event").color(Color32::WHITE).font(semibold(13.0))).fill(t.accent).min_size(vec2(212.0, 30.0));
        if ui.add(new).clicked() {
            let day = self.cal.day;
            self.new_event_at(day, None);
        }
        ui.add_space(10.0);
        // The small month: move about, and which days have something on.
        let tz = self.local_tz();
        let marks: HashSet<NaiveDate> = self.cal.cache.iter().map(|s| s.at.start.with_timezone(&tz).date_naive()).collect();
        let (lo, hi) = self.view_range();
        let mut mini = self.cal.mini;
        let mut picked = None;
        mini_month(ui, t, &mut mini, Some(self.cal.day), &|d| marks.contains(&d), Some((lo, hi)), &mut picked);
        self.cal.mini = mini;
        if let Some(d) = picked {
            self.cal.day = d;
            self.cal.popup = None;
        }
        ui.add_space(12.0);
        ui.label(RichText::new("Calendars").font(semibold(13.5)).color(t.text));
        ui.add_space(2.0);
        if self.cal.sources.is_empty() {
            ui.label(RichText::new("No calendar accounts yet.").font(regular(12.0)).color(t.text_dim));
        }
        let mut toggle = None;
        let mut sync = None;
        let mut make_new = None;
        egui::ScrollArea::vertical().id_salt("cal_list").max_height((ui.available_height() - 40.0).max(60.0)).show(ui, |ui| {
            for src in &self.cal.sources {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&src.label).font(semibold(12.0)).color(t.text_dim));
                    if self.cal.syncing.contains(&src.id) {
                        ui.spinner();
                    }
                });
                if let Some(Err(e)) = self.cal.status.get(&src.id) {
                    ui.add(egui::Label::new(RichText::new(e).font(regular(11.0)).color(t.red)).wrap());
                } else if !self.cal.syncing.contains(&src.id)
                    && src.can_make_calendars()
                    && !self.cal.cals.iter().any(|c| c.source == src.id)
                    && ui.link(RichText::new("No calendars yet: New calendar...").font(regular(12.0))).clicked()
                {
                    make_new = Some(src.id.clone());
                }
                for c in self.cal.cals.iter().filter(|c| c.source == src.id) {
                    let (rect, resp) = ui.allocate_exact_size(vec2(212.0, 24.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(rect, 3.0, t.hover);
                    }
                    let col = colour_of(Some(c));
                    let bx = Rect::from_center_size(rect.left_center() + vec2(12.0, 0.0), vec2(14.0, 14.0));
                    if c.shown {
                        ui.painter().rect_filled(bx, 3.0, col);
                        let ink = ink_on(col);
                        ui.painter().add(egui::Shape::line(
                            vec![bx.left_center() + vec2(3.0, 0.0), bx.center_bottom() + vec2(-1.0, -3.5), bx.right_top() + vec2(-3.0, 3.5)],
                            Stroke::new(1.6, ink),
                        ));
                    } else {
                        ui.painter().rect_stroke(bx, 3.0, Stroke::new(1.5, col), egui::StrokeKind::Inside);
                    }
                    let clip = ui.painter().with_clip_rect(rect);
                    let name = if c.read_only { format!("{} (read-only)", c.name) } else { c.name.clone() };
                    clip.text(rect.left_center() + vec2(28.0, 0.0), egui::Align2::LEFT_CENTER, name, regular(12.5), t.text);
                    if resp.clicked() {
                        toggle = Some((c.id, !c.shown));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Sync now").clicked() {
                            sync = Some(src.clone());
                            ui.close();
                        }
                    });
                }
                ui.add_space(6.0);
            }
        });
        if let Some((id, on)) = toggle
            && let Some(store) = &self.store
        {
            let _ = store.set_calendar_shown(id, on);
            self.cal.dirty = true;
            self.cal.upcoming_at = None;
        }
        if let Some(src) = sync {
            self.calendar_sync(&src);
        }
        if let Some(id) = make_new {
            self.cal.new_cal = Some((id, String::new(), 0));
            self.open_settings(crate::settings_ui::Page::Calendars);
        }
        let room = ui.available_height() - 28.0;
        if room > 0.0 {
            ui.add_space(room);
        }
        if ui.link(RichText::new("Calendar accounts...").font(regular(12.5))).clicked() {
            self.open_settings(crate::settings_ui::Page::Calendars);
        }
    }

    fn calendar_main(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let full = ui.max_rect();
        // The bar: Today, back and on, what is showing, and Day / Week / Month.
        let bar = Rect::from_min_size(full.min, vec2(full.width(), 44.0));
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(bar.shrink2(vec2(12.0, 8.0))).layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                let today = chrono::Local::now().date_naive();
                if ui.add(egui::Button::new(RichText::new("Today").font(regular(13.0))).min_size(vec2(64.0, 26.0))).clicked() {
                    self.cal.day = today;
                    self.cal.mini = first_of_month(today);
                    self.cal.scrolled = false;
                }
                let step = |d: NaiveDate, mode: Mode, n: i32| match mode {
                    Mode::Day => d + Span::days(n as i64),
                    Mode::Week => d + Span::days(7 * n as i64),
                    Mode::Month => add_months(d, n),
                };
                if ui
                    .add(egui::Button::new(RichText::new("\u{2039}").font(semibold(16.0))).min_size(vec2(28.0, 26.0)))
                    .on_hover_text("Back")
                    .clicked()
                {
                    self.cal.day = step(self.cal.day, self.cal.mode, -1);
                    self.cal.mini = first_of_month(self.cal.day);
                    self.cal.scrolled = false;
                }
                if ui.add(egui::Button::new(RichText::new("\u{203a}").font(semibold(16.0))).min_size(vec2(28.0, 26.0))).on_hover_text("On").clicked()
                {
                    self.cal.day = step(self.cal.day, self.cal.mode, 1);
                    self.cal.mini = first_of_month(self.cal.day);
                    self.cal.scrolled = false;
                }
                ui.add_space(8.0);
                let d = self.cal.day;
                let title = match self.cal.mode {
                    Mode::Day => d.format("%A %-d %B %Y").to_string(),
                    Mode::Week => {
                        let (a, b) = (week_start(d), week_start(d) + Span::days(6));
                        if a.month() == b.month() {
                            format!("{} \u{2013} {} {}", a.day(), b.day(), b.format("%B %Y"))
                        } else if a.year() == b.year() {
                            format!("{} \u{2013} {} {}", a.format("%-d %B"), b.format("%-d %B"), b.year())
                        } else {
                            format!("{} \u{2013} {}", a.format("%-d %B %Y"), b.format("%-d %B %Y"))
                        }
                    }
                    Mode::Month => d.format("%B %Y").to_string(),
                };
                ui.label(RichText::new(title).font(semibold(17.0)).color(t.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (m, label) in [(Mode::Month, "Month"), (Mode::Week, "Week"), (Mode::Day, "Day")] {
                        let on = self.cal.mode == m;
                        let b = egui::Button::new(RichText::new(label).font(if on { semibold(12.5) } else { regular(12.5) }).color(if on {
                            Color32::WHITE
                        } else {
                            t.text
                        }))
                        .fill(if on { t.accent } else { t.input })
                        .min_size(vec2(58.0, 26.0));
                        if ui.add(b).clicked() && !on {
                            self.cal.mode = m;
                            self.cal.scrolled = false;
                            self.cal.popup = None;
                        }
                    }
                    ui.add_space(10.0);
                    let busy = !self.cal.syncing.is_empty();
                    if busy {
                        ui.spinner();
                    } else if ui
                        .add(egui::Button::new(RichText::new("Sync").font(regular(12.5))).min_size(vec2(50.0, 26.0)))
                        .on_hover_text("Fetch changes from the calendar servers")
                        .clicked()
                    {
                        self.calendar_sync_all();
                    }
                });
            },
        );
        ui.painter().line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, t.border));
        let body = Rect::from_min_max(pos2(full.left(), bar.bottom() + 1.0), full.max);
        self.reload();
        match self.cal.mode {
            Mode::Month => self.month_grid(ui, t, body),
            Mode::Week => {
                let a = week_start(self.cal.day);
                self.time_grid(ui, t, body, (0..7).map(|i| a + Span::days(i)).collect());
            }
            Mode::Day => {
                let d = self.cal.day;
                self.time_grid(ui, t, body, vec![d]);
            }
        }
    }

    fn event_hit(&mut self, resp: &egui::Response, s: &Shown) {
        if resp.double_clicked() {
            let s = s.clone();
            self.edit_event(&s);
        } else if resp.clicked() {
            let at = resp.interact_pointer_pos().unwrap_or(resp.rect.center());
            self.cal.popup = Some((s.clone(), at, true));
        }
    }

    fn month_grid(&mut self, ui: &mut egui::Ui, t: &Tokens, body: Rect) {
        let tz = self.local_tz();
        let (first, _) = self.view_range();
        let month = self.cal.day.month();
        let today = chrono::Local::now().date_naive();
        let head_h = 26.0;
        let cols = 7.0;
        let cw = body.width() / cols;
        let rows = 6.0;
        let rh = (body.height() - head_h) / rows;
        let p = ui.painter().clone();
        for i in 0..7 {
            let x = body.left() + i as f32 * cw;
            let name = Weekday::try_from(i as u8).map(|w| format!("{w}")).unwrap_or_default();
            let long = match name.as_str() {
                "Mon" => "Monday",
                "Tue" => "Tuesday",
                "Wed" => "Wednesday",
                "Thu" => "Thursday",
                "Fri" => "Friday",
                "Sat" => "Saturday",
                _ => "Sunday",
            };
            p.text(
                pos2(x + 8.0, body.top() + head_h / 2.0),
                egui::Align2::LEFT_CENTER,
                if cw > 110.0 { long } else { &name },
                regular(12.0),
                t.text_dim,
            );
        }
        let cache = self.cal.cache.clone();
        let mut clicked_more = None;
        let mut new_at = None;
        for k in 0..42 {
            let d = first + Span::days(k);
            let (r, c) = ((k / 7) as f32, (k % 7) as f32);
            let cell = Rect::from_min_size(pos2(body.left() + c * cw, body.top() + head_h + r * rh), vec2(cw, rh));
            let resp = ui.interact(cell, ui.id().with(("mcell", k)), Sense::click());
            let out = d.month() != month;
            let bg = if d == self.cal.day && !out {
                t.checked
            } else if out {
                t.panel
            } else {
                t.ribbon
            };
            p.rect_filled(cell, 0.0, bg);
            p.rect_stroke(cell, 0.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
            let num = if d.day() == 1 { d.format("%-d %b").to_string() } else { d.day().to_string() };
            let num_pos = cell.left_top() + vec2(8.0, 5.0);
            if d == today {
                let w = p.layout_no_wrap(num.clone(), semibold(12.5), Color32::WHITE).size().x;
                p.rect_filled(Rect::from_min_size(num_pos - vec2(4.0, 2.0), vec2(w + 8.0, 19.0)), 9.0, t.accent);
                p.text(num_pos, egui::Align2::LEFT_TOP, num, semibold(12.5), Color32::WHITE);
            } else {
                p.text(num_pos, egui::Align2::LEFT_TOP, num, regular(12.5), if out { t.text_disabled } else { t.text });
            }
            if resp.double_clicked() {
                new_at = Some(d);
            } else if resp.clicked() {
                self.cal.day = d;
                self.cal.popup = None;
            }
            // The day's events: all-day ones as filled bars, then the timed ones.
            let (a, b) = (day_start(d, tz), day_start(d + Span::days(1), tz));
            let todays: Vec<&Shown> =
                cache.iter().filter(|s| s.at.start < b && (s.at.end > a || (s.at.end == s.at.start && s.at.start >= a))).collect();
            let line = 19.0;
            let fit = (((rh - 28.0) / line).floor() as usize).max(1);
            let show = if todays.len() > fit { fit.saturating_sub(1) } else { todays.len() };
            for (j, s) in todays.iter().take(show).enumerate() {
                let er = Rect::from_min_size(pos2(cell.left() + 4.0, cell.top() + 26.0 + j as f32 * line), vec2(cw - 8.0, line - 2.0));
                let col = colour_of(self.cal.cals.iter().find(|c| c.id == s.calendar_id));
                let er_resp = ui.interact(er, ui.id().with(("mev", k, j)), Sense::click());
                let clip = p.with_clip_rect(er);
                let tentative = s.event.status == "TENTATIVE";
                if s.event.start.is_date() || s.at.end - s.at.start >= Span::hours(24) {
                    clip.rect_filled(er, 3.0, if tentative { col.gamma_multiply(0.55) } else { col });
                    clip.text(er.left_center() + vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, &s.event.summary, regular(12.0), ink_on(col));
                } else {
                    if er_resp.hovered() {
                        clip.rect_filled(er, 3.0, t.hover);
                    }
                    clip.circle_filled(er.left_center() + vec2(6.0, 0.0), 3.5, col);
                    let time = hm(s.at.start, tz);
                    let tr = clip.text(er.left_center() + vec2(14.0, 0.0), egui::Align2::LEFT_CENTER, time, regular(11.5), t.text_dim);
                    clip.text(pos2(tr.right() + 5.0, er.center().y), egui::Align2::LEFT_CENTER, &s.event.summary, regular(12.0), t.text);
                }
                let s = (*s).clone();
                self.event_hit(&er_resp, &s);
            }
            if todays.len() > show {
                let mr = Rect::from_min_size(pos2(cell.left() + 4.0, cell.top() + 26.0 + show as f32 * line), vec2(cw - 8.0, line - 2.0));
                let more = ui.interact(mr, ui.id().with(("mmore", k)), Sense::click());
                p.text(
                    mr.left_center() + vec2(6.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{} more", todays.len() - show),
                    semibold(11.5),
                    if more.hovered() { t.accent } else { t.text_dim },
                );
                if more.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    clicked_more = Some(d);
                }
            }
        }
        if let Some(d) = clicked_more {
            self.cal.day = d;
            self.cal.mode = Mode::Day;
            self.cal.scrolled = false;
        }
        if let Some(d) = new_at {
            self.new_event_at(d, None);
        }
    }

    /// The week or day: a column per day, an hour a row, all-day events along the top.
    fn time_grid(&mut self, ui: &mut egui::Ui, t: &Tokens, body: Rect, days: Vec<NaiveDate>) {
        let tz = self.local_tz();
        let today = chrono::Local::now().date_naive();
        let n = days.len().max(1) as f32;
        let cw = (body.width() - GUTTER) / n;
        let cache = self.cal.cache.clone();
        let p = ui.painter().clone();
        let dark = t.ribbon.r() < 100;
        // Day headings.
        let head = Rect::from_min_size(body.min, vec2(body.width(), 40.0));
        for (i, d) in days.iter().enumerate() {
            let x = body.left() + GUTTER + i as f32 * cw;
            let on = *d == today;
            p.text(
                pos2(x + 8.0, head.top() + 6.0),
                egui::Align2::LEFT_TOP,
                d.format("%a").to_string(),
                regular(11.5),
                if on { t.accent } else { t.text_dim },
            );
            p.text(
                pos2(x + 8.0, head.top() + 19.0),
                egui::Align2::LEFT_TOP,
                d.format("%-d").to_string(),
                semibold(16.0),
                if on { t.accent } else { t.text },
            );
        }
        // All-day events (and those running a day or more).
        let long = |s: &Shown| s.event.start.is_date() || s.at.end - s.at.start >= Span::hours(24);
        let mut rows_by_day: Vec<Vec<&Shown>> = vec![vec![]; days.len()];
        for s in cache.iter().filter(|s| long(s)) {
            for (i, d) in days.iter().enumerate() {
                if s.at.start < day_start(*d + Span::days(1), tz)
                    && s.at.end > day_start(*d, tz)
                    && let Some(v) = rows_by_day.get_mut(i)
                {
                    v.push(s);
                }
            }
        }
        let most = rows_by_day.iter().map(|v| v.len()).max().unwrap_or(0);
        let allday_h = (most as f32 * 20.0 + 6.0).max(24.0);
        let allday = Rect::from_min_size(pos2(body.left(), head.bottom()), vec2(body.width(), allday_h));
        p.text(allday.left_center() + vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, "All day", regular(11.0), t.text_dim);
        for (i, list) in rows_by_day.iter().enumerate() {
            let x = body.left() + GUTTER + i as f32 * cw;
            let cell = Rect::from_min_size(pos2(x, allday.top()), vec2(cw, allday_h));
            let cresp = ui.interact(cell, ui.id().with(("adcell", i)), Sense::click());
            if cresp.double_clicked()
                && let Some(d) = days.get(i)
            {
                self.new_event_at(*d, None);
                if let Some(f) = self.cal.editor.as_mut() {
                    f.all_day = true;
                }
            }
            for (j, s) in list.iter().enumerate() {
                let er = Rect::from_min_size(pos2(x + 2.0, allday.top() + 3.0 + j as f32 * 20.0), vec2(cw - 4.0, 18.0));
                let col = colour_of(self.cal.cals.iter().find(|c| c.id == s.calendar_id));
                let clip = p.with_clip_rect(er);
                clip.rect_filled(er, 3.0, col);
                clip.text(er.left_center() + vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, &s.event.summary, regular(12.0), ink_on(col));
                let r = ui.interact(er, ui.id().with(("adev", i, j)), Sense::click());
                let s = (*s).clone();
                self.event_hit(&r, &s);
            }
        }
        p.line_segment([allday.left_bottom(), allday.right_bottom()], Stroke::new(1.0, t.border_strong));
        // The hours.
        let grid_top = allday.bottom() + 1.0;
        let view = Rect::from_min_max(pos2(body.left(), grid_top), body.max);
        let mut scroll = egui::ScrollArea::vertical().id_salt(("cal_hours", days.len())).auto_shrink([false, false]);
        if !self.cal.scrolled {
            // To the morning, or (today, later on) to an hour before now.
            let now_h = chrono::Local::now().hour() as f32;
            let first = if days.contains(&today) { (now_h - 1.0).clamp(7.0, 16.0) } else { 7.0 };
            scroll = scroll.vertical_scroll_offset(first * HOUR);
            self.cal.scrolled = true;
        }
        let mut new_at: Option<(NaiveDate, NaiveTime)> = None;
        let mut hits: Vec<(egui::Response, Shown)> = vec![];
        ui.scope_builder(egui::UiBuilder::new().max_rect(view), |ui| {
            scroll.show(ui, |ui| {
                let (area, resp) = ui.allocate_exact_size(vec2(view.width(), 24.0 * HOUR), Sense::click());
                let p = ui.painter();
                for h in 0..24 {
                    let y = area.top() + h as f32 * HOUR;
                    p.line_segment([pos2(area.left() + GUTTER - 6.0, y), pos2(area.right(), y)], Stroke::new(1.0, t.border));
                    p.line_segment(
                        [pos2(area.left() + GUTTER, y + HOUR / 2.0), pos2(area.right(), y + HOUR / 2.0)],
                        Stroke::new(1.0, t.border.gamma_multiply(0.45)),
                    );
                    if h > 0 {
                        p.text(pos2(area.left() + GUTTER - 10.0, y), egui::Align2::RIGHT_CENTER, format!("{h:02}:00"), regular(11.0), t.text_dim);
                    }
                }
                for i in 0..=days.len() {
                    let x = area.left() + GUTTER + i as f32 * cw;
                    p.line_segment([pos2(x, area.top()), pos2(x, area.bottom())], Stroke::new(1.0, t.border));
                }
                if resp.double_clicked()
                    && let Some(pos) = resp.interact_pointer_pos()
                {
                    let i = ((pos.x - area.left() - GUTTER) / cw).floor();
                    if i >= 0.0
                        && let Some(d) = days.get(i as usize)
                    {
                        let half = (((pos.y - area.top()) / (HOUR / 2.0)).floor() as u32).min(47);
                        new_at = NaiveTime::from_hms_opt(half / 2, (half % 2) * 30, 0).map(|tm| (*d, tm));
                    }
                }
                for (i, d) in days.iter().enumerate() {
                    let x0 = area.left() + GUTTER + i as f32 * cw;
                    let (a, b) = (day_start(*d, tz), day_start(*d + Span::days(1), tz));
                    let y_of = |t: DateTime<Utc>| area.top() + ((t - a).num_seconds() as f32 / 3600.0) * HOUR;
                    // The day's timed events, side by side where they overlap.
                    let mut evs: Vec<(&Shown, DateTime<Utc>, DateTime<Utc>)> = cache
                        .iter()
                        .filter(|s| !long(s) && s.at.start < b && (s.at.end > a || (s.at.end == s.at.start && s.at.start >= a)))
                        .map(|s| (s, s.at.start.max(a), s.at.end.min(b).max(s.at.start.max(a) + Span::minutes(25))))
                        .collect();
                    evs.sort_by_key(|(_, s, e)| (*s, std::cmp::Reverse(*e)));
                    let mut col_of: Vec<usize> = vec![0; evs.len()];
                    let mut cols_in: Vec<usize> = vec![1; evs.len()];
                    let mut k = 0;
                    while k < evs.len() {
                        // A cluster: events chained by overlapping.
                        let mut end = evs.get(k).map(|e| e.2).unwrap_or(a);
                        let mut j = k + 1;
                        while j < evs.len() && evs.get(j).is_some_and(|e| e.1 < end) {
                            end = end.max(evs.get(j).map(|e| e.2).unwrap_or(end));
                            j += 1;
                        }
                        let mut lanes: Vec<DateTime<Utc>> = vec![];
                        for m in k..j {
                            let Some(e) = evs.get(m) else { continue };
                            let lane = match lanes.iter().position(|l| *l <= e.1) {
                                Some(l) => l,
                                None => {
                                    lanes.push(e.2);
                                    lanes.len() - 1
                                }
                            };
                            if let Some(l) = lanes.get_mut(lane) {
                                *l = e.2;
                            }
                            if let Some(c) = col_of.get_mut(m) {
                                *c = lane;
                            }
                        }
                        for m in k..j {
                            if let Some(c) = cols_in.get_mut(m) {
                                *c = lanes.len().max(1);
                            }
                        }
                        k = j;
                    }
                    for (m, (s, s0, s1)) in evs.iter().enumerate() {
                        let lanes = cols_in.get(m).copied().unwrap_or(1) as f32;
                        let lane = col_of.get(m).copied().unwrap_or(0) as f32;
                        let w = (cw - 6.0) / lanes;
                        let er =
                            Rect::from_min_max(pos2(x0 + 2.0 + lane * w, y_of(*s0) + 1.0), pos2(x0 + 2.0 + (lane + 1.0) * w - 2.0, y_of(*s1) - 1.0));
                        let col = colour_of(self.cal.cals.iter().find(|c| c.id == s.calendar_id));
                        let tentative = s.event.status == "TENTATIVE";
                        let clip = p.with_clip_rect(er);
                        // Tinted: lighter on a light background, stronger on a dark one so it still shows.
                        let tint = if dark { 0.55 } else { 0.28 } * if tentative { 0.6 } else { 1.0 };
                        clip.rect_filled(er, 4.0, col.gamma_multiply(tint));
                        clip.rect_filled(Rect::from_min_size(er.min, vec2(4.0, er.height())), 2.0, col);
                        let title = if s.event.summary.is_empty() { "(no title)" } else { &s.event.summary };
                        let tr = clip.text(er.left_top() + vec2(9.0, 3.0), egui::Align2::LEFT_TOP, title, semibold(12.0), t.text);
                        if er.height() > 34.0 {
                            let mut sub = format!("{} \u{2013} {}", hm(s.at.start, tz), hm(s.at.end, tz));
                            if !s.event.location.is_empty() {
                                sub.push_str(&format!("  \u{00b7}  {}", s.event.location));
                            }
                            clip.text(pos2(er.left() + 9.0, tr.bottom() + 2.0), egui::Align2::LEFT_TOP, sub, regular(11.0), t.text_dim);
                        }
                        let r = ui.interact(er, ui.id().with(("tev", i, m)), Sense::click());
                        hits.push((r, (*s).clone()));
                    }
                    // Now, as a line across today.
                    if *d == today {
                        let y = y_of(Utc::now());
                        p.line_segment([pos2(x0, y), pos2(x0 + cw, y)], Stroke::new(2.0, t.red));
                        p.circle_filled(pos2(x0, y), 4.0, t.red);
                    }
                }
            });
        });
        for (r, s) in hits {
            self.event_hit(&r, &s);
        }
        if let Some((d, tm)) = new_at {
            self.new_event_at(d, Some(tm));
        }
        ui.ctx().request_repaint_after(Duration::from_secs(60));
    }

    /// An event's details, beside where it was clicked.
    fn event_popup(&mut self, ctx: &egui::Context, t: &Tokens) {
        let Some((s, at, fresh)) = self.cal.popup.clone() else { return };
        let tz = self.local_tz();
        let cal = self.cal.cals.iter().find(|c| c.id == s.calendar_id).cloned();
        let screen = ctx.content_rect();
        let w = 340.0;
        let pos = pos2(
            (at.x + 12.0).min(screen.right() - w - 12.0),
            (at.y - 20.0).clamp(screen.top() + 8.0, (screen.bottom() - 300.0).max(screen.top() + 8.0)),
        );
        let mut edit = false;
        let mut delete: Option<bool> = None;
        let mut close = false;
        let shown = egui::Area::new(egui::Id::new("cal_popup")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).fill(t.menu).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
                ui.set_width(w - 28.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(if s.event.summary.is_empty() { "(no title)" } else { &s.event.summary })
                                .font(semibold(15.0))
                                .color(t.text),
                        )
                        .wrap(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        if crate::people::cross_button(ui, t).on_hover_text("Close").clicked() {
                            close = true;
                        }
                    });
                });
                ui.add_space(4.0);
                ui.label(RichText::new(when_text(&s, tz)).font(regular(12.5)).color(t.text));
                if s.event.rrule.is_some() || s.at.instance.is_some() {
                    let d = s.at.start.with_timezone(&tz).date_naive();
                    ui.label(RichText::new(Repeat::of(s.event.rrule.as_deref()).label(d)).font(regular(12.0)).color(t.text_dim));
                }
                if !s.event.location.is_empty() {
                    ui.label(RichText::new(&s.event.location).font(regular(12.5)).color(t.text));
                }
                if let Some(c) = &cal {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 5.0, colour_of(Some(c)));
                        ui.label(RichText::new(&c.name).font(regular(12.0)).color(t.text_dim));
                    });
                }
                if let Some(m) = s.event.alarms.first() {
                    ui.label(RichText::new(format!("Reminder: {}", reminder_label(*m))).font(regular(12.0)).color(t.text_dim));
                }
                if let Some(o) = &s.event.organizer {
                    let who = if o.name.is_empty() { o.email.clone() } else { o.name.clone() };
                    ui.label(RichText::new(format!("Organiser: {who}")).font(regular(12.0)).color(t.text_dim));
                }
                if !s.event.description.trim().is_empty() {
                    ui.add_space(4.0);
                    let notes: String = s.event.description.lines().take(8).collect::<Vec<_>>().join("\n");
                    ui.add(egui::Label::new(RichText::new(notes).font(regular(12.0)).color(t.text)).wrap());
                }
                ui.add_space(8.0);
                let writable = cal.as_ref().is_some_and(|c| !c.read_only);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(writable, |ui| {
                        if ui.button("Edit").clicked() {
                            edit = true;
                        }
                        if s.at.instance.is_some() && s.event.recurrence_id.is_none() {
                            if ui.button("Delete this one").clicked() {
                                delete = Some(true);
                            }
                            if ui.button("Delete the series").clicked() {
                                delete = Some(false);
                            }
                        } else if ui.button("Delete").clicked() {
                            delete = Some(false);
                        }
                    });
                });
            });
        });
        // A click anywhere else closes it (not the click that opened it).
        let outside = ctx.input(|i| i.pointer.any_pressed()) && !ctx.pointer_interact_pos().is_some_and(|p| shown.response.rect.contains(p));
        if let Some(pp) = self.cal.popup.as_mut() {
            pp.2 = false;
        }
        if edit {
            self.edit_event(&s);
        } else if let Some(only) = delete {
            self.delete_event(&s, only);
        } else if close || (outside && !fresh) || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cal.popup = None;
        }
    }

    fn event_editor(&mut self, ctx: &egui::Context, t: &Tokens) {
        let Some(mut f) = self.cal.editor.take() else { return };
        let writable: Vec<(i64, String, Color32)> = self
            .cal
            .cals
            .iter()
            .filter(|c| !c.read_only)
            .map(|c| {
                let src = self.cal.sources.iter().find(|s| s.id == c.source).map(|s| s.label.clone()).unwrap_or_default();
                (c.id, format!("{} \u{00b7} {src}", c.name), colour_of(Some(c)))
            })
            .collect();
        let mut save = false;
        let mut cancel = false;
        let title = if f.row.is_some() { "Edit event" } else { "New event" };
        egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
            ui.set_width(470.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 7.0);
            let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
                ui.horizontal(|ui| {
                    ui.add_sized([86.0, 22.0], egui::Label::new(RichText::new(label).color(t.text_dim)));
                    add(ui);
                });
            };
            let title_edit = ui.add_sized(
                [470.0, 28.0],
                egui::TextEdit::singleline(&mut f.title).id(egui::Id::new("cal_title")).hint_text("Add a title").font(semibold(15.0)),
            );
            if f.row.is_none() && f.title.is_empty() && !title_edit.has_focus() && ui.memory(|m| m.focused().is_none()) {
                title_edit.request_focus();
            }
            row(ui, "Calendar", &mut |ui| {
                let current = writable.iter().find(|c| c.0 == f.calendar).map(|c| c.1.clone()).unwrap_or_default();
                egui::ComboBox::from_id_salt("cal_pick").width(360.0).selected_text(current).show_ui(ui, |ui| {
                    for (id, name, col) in &writable {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                            ui.painter().circle_filled(r.center(), 5.0, *col);
                            ui.selectable_value(&mut f.calendar, *id, name);
                        });
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.add_space(94.0);
                ui.checkbox(&mut f.all_day, "All day");
            });
            let (old_start, old_time) = (f.start_date, f.start_time);
            let all_day = f.all_day;
            for which in [true, false] {
                row(ui, if which { "Starts" } else { "Ends" }, &mut |ui| {
                    let (date, time) = if which { (&mut f.start_date, &mut f.start_time) } else { (&mut f.end_date, &mut f.end_time) };
                    let open = f.picker.is_some_and(|(w, _)| w == which);
                    if ui.add(egui::Button::new(date.format("%a %-d %b %Y").to_string()).min_size(vec2(130.0, 24.0)).selected(open)).clicked() {
                        f.picker = if open { None } else { Some((which, first_of_month(*date))) };
                    }
                    if !all_day {
                        time_combo(ui, if which { "cal_t0" } else { "cal_t1" }, time);
                    }
                });
                if let Some((w, month)) = f.picker
                    && w == which
                {
                    ui.horizontal(|ui| {
                        ui.add_space(94.0);
                        ui.vertical(|ui| {
                            let mut m = month;
                            let mut picked = None;
                            let cur = if which { f.start_date } else { f.end_date };
                            mini_month(ui, t, &mut m, Some(cur), &|_| false, None, &mut picked);
                            f.picker = Some((w, m));
                            if let Some(d) = picked {
                                if which {
                                    f.start_date = d;
                                } else {
                                    f.end_date = d;
                                }
                                f.picker = None;
                            }
                        });
                    });
                }
            }
            // Moving the start moves the end with it (the length stays).
            if (f.start_date, f.start_time) != (old_start, old_time) {
                let moved = f.start_date.and_time(f.start_time) - old_start.and_time(old_time);
                let end = f.end_date.and_time(f.end_time) + moved;
                f.end_date = end.date();
                f.end_time = end.time();
            }
            row(ui, "Repeat", &mut |ui| {
                let d = f.start_date;
                let mut choices = vec![Repeat::None, Repeat::Daily, Repeat::Weekdays, Repeat::Weekly, Repeat::Monthly, Repeat::Yearly];
                if let Repeat::Custom(_) = f.repeat {
                    choices.push(f.repeat.clone());
                }
                egui::ComboBox::from_id_salt("cal_repeat").width(360.0).selected_text(f.repeat.label(d)).show_ui(ui, |ui| {
                    for c in choices {
                        let l = c.label(d);
                        ui.selectable_value(&mut f.repeat, c, l);
                    }
                });
            });
            if f.instance.is_some() {
                ui.horizontal(|ui| {
                    ui.add_space(94.0);
                    ui.label(RichText::new("Changes apply to every time it repeats.").font(regular(11.5)).color(t.text_dim));
                });
            }
            row(ui, "Reminder", &mut |ui| {
                let label = f.reminder.map(reminder_label).unwrap_or_else(|| "None".into());
                egui::ComboBox::from_id_salt("cal_rem").width(200.0).selected_text(label).show_ui(ui, |ui| {
                    for (v, l) in REMINDERS {
                        ui.selectable_value(&mut f.reminder, *v, *l);
                    }
                });
            });
            row(ui, "Location", &mut |ui| {
                ui.add_sized([370.0, 24.0], egui::TextEdit::singleline(&mut f.location).id(egui::Id::new("cal_loc")));
            });
            match &f.invited_by {
                Some(who) => row(ui, "Invited by", &mut |ui| {
                    ui.label(RichText::new(who).color(t.text_dim));
                }),
                None => {
                    row(ui, "Invite", &mut |ui| {
                        ui.add_sized(
                            [370.0, 24.0],
                            egui::TextEdit::singleline(&mut f.invite).id(egui::Id::new("cal_invite")).hint_text("Addresses, separated by commas"),
                        );
                    });
                    if !f.invite.trim().is_empty() {
                        ui.horizontal(|ui| {
                            ui.add_space(94.0);
                            ui.label(
                                RichText::new("They are sent an invitation when you save, and updates after.").font(regular(11.5)).color(t.text_dim),
                            );
                        });
                    }
                }
            }
            ui.label(RichText::new("Notes").color(t.text_dim));
            ui.add_sized([470.0, 90.0], egui::TextEdit::multiline(&mut f.notes).id(egui::Id::new("cal_notes")));
            if let Some(e) = &f.error {
                ui.add(egui::Label::new(RichText::new(e).color(t.red)).wrap());
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!f.busy && !writable.is_empty(), |ui| {
                    let b =
                        egui::Button::new(RichText::new("Save").color(Color32::WHITE).font(semibold(13.0))).fill(t.accent).min_size(vec2(80.0, 28.0));
                    if ui.add(b).clicked() {
                        save = true;
                    }
                });
                if ui.add(egui::Button::new("Cancel").min_size(vec2(80.0, 28.0))).clicked() {
                    cancel = true;
                }
                if f.busy {
                    ui.spinner();
                    ui.label(RichText::new("Saving...").color(t.text_dim));
                }
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
        });
        if cancel {
            return;
        }
        if save {
            match self.event_from_form(&f) {
                Ok(e) => {
                    f.error = None;
                    f.busy = true;
                    let (cal, row, from) = (f.calendar, f.row, f.from_calendar);
                    // A new calendar for an existing event: write it there, then delete the old.
                    let r = match (row, from) {
                        (Some(old), Some(was)) if was != cal => {
                            let mut fresh = e.clone();
                            fresh.uid = calstore::new_uid();
                            let itips = self.meeting_message("REQUEST", cal, &fresh).into_iter().collect();
                            self.cal_send(cal, |src, pw| CalRequest::Save(src, pw, cal, None, Box::new(fresh), itips))
                                .and_then(|_| self.cal_send(was, |src, pw| CalRequest::Delete(src, pw, old, vec![])))
                        }
                        _ => {
                            let mut itips: Vec<Itip> = self.meeting_message("REQUEST", cal, &e).into_iter().collect();
                            // Guests taken off the list are told it is off for them.
                            if row.is_some() {
                                itips.extend(self.removed_guests_message(cal, &f.base, &e));
                            }
                            self.cal_send(cal, |src, pw| CalRequest::Save(src, pw, cal, row, Box::new(e), itips))
                        }
                    };
                    if let Err(e) = r {
                        f.busy = false;
                        f.error = Some(e);
                    }
                }
                Err(e) => f.error = Some(e),
            }
        }
        self.cal.editor = Some(f);
    }

    fn event_from_form(&self, f: &EventForm) -> Result<Event, String> {
        let tz = self.local_tz();
        let mut e = if f.row.is_some() { f.base.clone() } else { Event { uid: calstore::new_uid(), ..Default::default() } };
        e.summary = f.title.trim().to_string();
        e.location = f.location.trim().to_string();
        e.description = f.notes.clone();
        if f.invited_by.is_none() {
            let wanted: Vec<String> =
                f.invite.split([',', ';', ' ', '\n']).map(|a| a.trim().trim_matches(['<', '>']).to_string()).filter(|a| !a.is_empty()).collect();
            if let Some(bad) = wanted.iter().find(|a| !a.contains('@') || a.starts_with('@') || a.ends_with('@')) {
                return Err(format!("\"{bad}\" is not an email address."));
            }
            if wanted.is_empty() {
                // No one invited (any more): just mine.
                if e.organizer.as_ref().is_some_and(|o| self.is_me(&o.email, f.calendar)) {
                    e.organizer = None;
                }
                e.attendees.clear();
            } else {
                let me = self.my_address(f.calendar);
                let name = self.settings.name_for(&me).unwrap_or_default();
                let old = std::mem::take(&mut e.attendees);
                e.attendees = wanted
                    .iter()
                    .map(|w| {
                        old.iter().find(|a| a.email.eq_ignore_ascii_case(w)).cloned().unwrap_or(ical::Attendee {
                            email: w.clone(),
                            name: String::new(),
                            partstat: "NEEDS-ACTION".into(),
                        })
                    })
                    .collect();
                // (A change to an existing event already counts up its SEQUENCE, below.)
                e.organizer = Some(ical::Attendee { email: me, name, partstat: String::new() });
            }
        }
        let old_rule = e.rrule.clone();
        e.rrule = f.repeat.rule();
        if f.all_day {
            if f.end_date < f.start_date {
                return Err("The event ends before it starts.".into());
            }
            let (s, en) = (When::Date(f.start_date), When::Date(f.end_date + Span::days(1)));
            e.start = s;
            e.end = en;
            e.tzid = None;
            if let (Some(inst), true) = (f.instance, old_rule.is_some() && f.base.start.is_date()) {
                // The series moves by as many days as this occurrence did.
                let days = (f.start_date - inst.with_timezone(&tz).date_naive()).num_days();
                if let When::Date(b) = f.base.start {
                    let len = (f.end_date - f.start_date).num_days() + 1;
                    e.start = When::Date(b + Span::days(days));
                    e.end = When::Date(b + Span::days(days + len));
                }
            }
        } else {
            let local = |d: NaiveDate, tm: NaiveTime| tz.from_local_datetime(&d.and_time(tm)).earliest().map(|x| x.with_timezone(&Utc));
            let s = local(f.start_date, f.start_time).ok_or("That start time does not exist (the clocks change then).")?;
            let en = local(f.end_date, f.end_time).ok_or("That end time does not exist (the clocks change then).")?;
            if en < s {
                return Err("The event ends before it starts.".into());
            }
            let (s, en) = match f.instance {
                // The series moves by as much as this occurrence did.
                Some(inst) if old_rule.is_some() && !f.base.start.is_date() => {
                    let start = f.base.start.utc(tz) + (s - inst);
                    (start, start + (en - s))
                }
                _ => (s, en),
            };
            e.start = When::Time(s);
            e.end = When::Time(en);
            e.tzid = ical::local_zone_name();
        }
        if f.reminder != f.base.alarms.first().copied() || f.row.is_none() {
            e.alarms = f.reminder.into_iter().collect();
        }
        if f.row.is_some() {
            e.sequence += 1;
        }
        Ok(e)
    }

    /// Account settings > Calendars: the calendar accounts, and the form adding or changing one.
    pub(crate) fn calendars_section(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let note = |ui: &mut egui::Ui, s: &str| {
            ui.add(egui::Label::new(RichText::new(s).font(regular(11.5)).color(t.text_dim)).wrap());
        };
        note(
            ui,
            "Calendars from your mail server, Nextcloud, Google, Outlook.com / Microsoft 365 or any CalDAV server show in the Calendar (the calendar button at the left of the window).",
        );
        ui.add_space(6.0);
        let mut remove = None;
        let mut edit = None;
        let mut sync = None;
        let mut new_cal = None;
        let mut make: Option<(CalSource, String, usize)> = None;
        let have: Vec<String> = self.store.as_ref().and_then(|s| s.calendars().ok()).unwrap_or_default().into_iter().map(|c| c.source).collect();
        let mut cancel_new = false;
        for src in &self.cal.sources {
            egui::Frame::NONE.stroke(Stroke::new(1.0, t.border)).corner_radius(4.0).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                ui.set_width(ui.available_width().min(620.0));
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&src.label).font(semibold(13.5)).color(t.text));
                    let kind = match src.kind {
                        SourceKind::MailServer => "mail server",
                        SourceKind::Nextcloud => "Nextcloud",
                        SourceKind::CalDav => "CalDAV",
                        SourceKind::Google => "Google",
                        SourceKind::Microsoft => "Microsoft",
                    };
                    ui.label(
                        RichText::new(format!("{kind} \u{00b7} {} \u{00b7} {}", src.server, src.username)).font(regular(12.0)).color(t.text_dim),
                    );
                });
                match self.cal.status.get(&src.id) {
                    Some(Ok(s)) => note(ui, s),
                    Some(Err(e)) => {
                        ui.add(egui::Label::new(RichText::new(e).font(regular(12.0)).color(t.red)).wrap());
                    }
                    None => {}
                }
                ui.horizontal(|ui| {
                    if self.cal.syncing.contains(&src.id) {
                        ui.spinner();
                    } else if ui.button("Sync now").clicked() {
                        sync = Some(src.clone());
                    }
                    if ui.button("Edit...").clicked() {
                        edit = Some(src.clone());
                    }
                    if src.can_make_calendars() && ui.button("New calendar...").clicked() && self.cal.new_cal.is_none() {
                        new_cal = Some(src.id.clone());
                    }
                    if ui.button("Remove").clicked() {
                        remove = Some(src.id.clone());
                    }
                });
                let none_yet = !self.cal.syncing.contains(&src.id) && matches!(self.cal.status.get(&src.id), Some(Ok(_))) && !have.contains(&src.id);
                if none_yet && self.cal.new_cal.is_none() && src.can_make_calendars() {
                    note(ui, "There are no calendars on this server yet: make one with New calendar.");
                }
                // Naming the new calendar and picking its colour.
                if let Some((_, name, colour)) = self.cal.new_cal.as_mut().filter(|n| n.0 == src.id) {
                    let mut enter = false;
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        let r = ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(name).id(egui::Id::new("cal_new_name")).hint_text("Calendar"));
                        if name.is_empty() && !r.has_focus() && ui.memory(|m| m.focused().is_none()) {
                            r.request_focus();
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !name.trim().is_empty() {
                            enter = true;
                        }
                        for (k, (rr, gg, bb)) in PALETTE.iter().enumerate() {
                            let (sw, resp) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
                            ui.painter().circle_filled(sw.center(), 8.0, Color32::from_rgb(*rr, *gg, *bb));
                            if *colour == k {
                                ui.painter().circle_stroke(sw.center(), 10.0, Stroke::new(1.5, t.text));
                            }
                            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                *colour = k;
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        let ok = !name.trim().is_empty();
                        if ui.add_enabled(ok, egui::Button::new(RichText::new("Create").color(Color32::WHITE)).fill(t.accent)).clicked() || enter {
                            make = Some((src.clone(), name.trim().to_string(), *colour));
                        }
                        if ui.button("Cancel").clicked() {
                            cancel_new = true;
                        }
                    });
                }
            });
            ui.add_space(4.0);
        }
        if let Some(src) = sync {
            self.calendar_sync(&src);
        }
        if let Some(id) = new_cal {
            self.cal.new_cal = Some((id, String::new(), 0));
        }
        if cancel_new {
            self.cal.new_cal = None;
        }
        if let Some((src, name, k)) = make {
            let (r, g, b) = PALETTE.get(k).copied().unwrap_or(PALETTE[0]);
            let colour = format!("#{r:02X}{g:02X}{b:02X}");
            match self.cal_secret(&src) {
                Some(pw) => {
                    self.cal.new_cal = None;
                    self.cal.syncing.insert(src.id.clone());
                    self.cal.status.insert(src.id.clone(), Ok(format!("Making {name}...")));
                    let _ = self.worker.send(crate::worker::Request::Cal(CalRequest::MakeCalendar(src, pw, name, Some(colour))));
                }
                None => {
                    self.cal.status.insert(src.id.clone(), Err(src.missing_secret()));
                }
            }
        }
        if let Some(src) = edit {
            self.cal.form = Some(SourceForm {
                editing: Some(src.id.clone()),
                kind: src.kind,
                account: src.account.clone(),
                label: src.label.clone(),
                server: src.server.clone(),
                username: src.username.clone(),
                password: String::new(),
                allow_insecure: src.allow_insecure,
                server_sends_invites: src.server_sends_invites,
                id: src.id.clone(),
                busy: false,
                result: None,
                found: vec![],
                home: src.home.clone(),
                signing_in: false,
            });
        }
        if let Some(id) = remove {
            self.cal.sources.retain(|s| s.id != id);
            if let Some(store) = &self.store {
                let _ = store.prune_calendars(&id, &[]);
            }
            crate::secrets::forget(&format!("calendar:{id}"));
            if let Err(e) = save_sources(&self.cal.sources) {
                self.status = format!("Could not save the calendar accounts: {e}");
            }
            self.cal.dirty = true;
        }
        if self.cal.form.is_none() {
            ui.add_space(6.0);
            if ui.add(egui::Button::new(RichText::new("+  Add a calendar account").font(semibold(13.0))).min_size(vec2(200.0, 28.0))).clicked() {
                let acct = self.accounts.iter().find(|a| a.oauth.is_none()).cloned();
                let mut id_bytes = calstore::new_uid();
                id_bytes.truncate(12);
                self.cal.form = Some(SourceForm {
                    editing: None,
                    kind: if acct.is_some() { SourceKind::MailServer } else { SourceKind::Nextcloud },
                    account: acct.as_ref().map(|a| a.email.clone()).unwrap_or_default(),
                    label: String::new(),
                    server: acct.as_ref().map(|a| a.imap.host.clone()).unwrap_or_default(),
                    username: acct.as_ref().map(|a| a.username.clone()).unwrap_or_default(),
                    password: String::new(),
                    allow_insecure: false,
                    server_sends_invites: false,
                    id: id_bytes,
                    busy: false,
                    result: None,
                    found: vec![],
                    home: String::new(),
                    signing_in: false,
                });
            }
            return;
        }
        self.source_form_ui(ui, t);
    }

    fn source_form_ui(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let accounts: Vec<(String, String, String)> =
            self.accounts.iter().filter(|a| a.oauth.is_none()).map(|a| (a.email.clone(), a.imap.host.clone(), a.username.clone())).collect();
        let Some(f) = self.cal.form.as_mut() else { return };
        let mut test = false;
        let mut save = false;
        let mut cancel = false;
        ui.separator();
        ui.label(
            RichText::new(if f.editing.is_some() { "Change the calendar account" } else { "Add a calendar account" })
                .font(semibold(14.5))
                .color(t.text),
        );
        let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
            ui.horizontal(|ui| {
                ui.add_sized([150.0, 20.0], egui::Label::new(label));
                add(ui);
            });
        };
        let before = f.kind;
        ui.horizontal(|ui| {
            ui.add_space(154.0);
            ui.vertical(|ui| {
                ui.add_enabled_ui(!accounts.is_empty(), |ui| ui.radio_value(&mut f.kind, SourceKind::MailServer, "My mail server's calendar"));
                ui.radio_value(&mut f.kind, SourceKind::Nextcloud, "Nextcloud");
                ui.radio_value(&mut f.kind, SourceKind::Google, "Google Calendar");
                ui.radio_value(&mut f.kind, SourceKind::Microsoft, "Outlook.com or Microsoft 365");
                ui.radio_value(&mut f.kind, SourceKind::CalDav, "Another CalDAV server (Radicale, Baikal, Fastmail, iCloud...)");
            });
        });
        if f.kind != before {
            f.result = None;
            f.found.clear();
            f.home.clear();
            if f.kind != SourceKind::MailServer && before == SourceKind::MailServer {
                f.server.clear();
            }
            match f.kind {
                SourceKind::Google => f.server = "apidata.googleusercontent.com".into(),
                SourceKind::Microsoft => f.server = "graph.microsoft.com".into(),
                _ if matches!(before, SourceKind::Google | SourceKind::Microsoft) => f.server.clear(),
                _ => {}
            }
            if matches!(f.kind, SourceKind::Google | SourceKind::Microsoft) {
                f.allow_insecure = false;
            }
        }
        if f.kind == SourceKind::MailServer {
            row(ui, "Mail account", &mut |ui| {
                let prev = f.account.clone();
                egui::ComboBox::from_id_salt("cal_acct").width(300.0).selected_text(f.account.clone()).show_ui(ui, |ui| {
                    for (email, _, _) in &accounts {
                        ui.selectable_value(&mut f.account, email.clone(), email);
                    }
                });
                if f.account != prev
                    && let Some((_, host, user)) = accounts.iter().find(|a| a.0 == f.account)
                {
                    f.server = host.clone();
                    f.username = user.clone();
                }
            });
        }
        row(ui, "Name", &mut |ui| {
            let hint = match f.kind {
                SourceKind::MailServer => f.account.clone(),
                SourceKind::Nextcloud => "Nextcloud".into(),
                SourceKind::CalDav => "Calendar".into(),
                SourceKind::Google => "Google".into(),
                SourceKind::Microsoft => "Outlook".into(),
            };
            ui.add_sized([300.0, 22.0], egui::TextEdit::singleline(&mut f.label).id(egui::Id::new("cal_src_label")).hint_text(hint));
        });
        // Google's or Microsoft's addresses typed into a plain CalDAV form: they need their own sign-in.
        let typed = f.server.to_ascii_lowercase();
        if matches!(f.kind, SourceKind::CalDav | SourceKind::Nextcloud) {
            let google = ["google.com", "googleusercontent.com", "gmail.com", "googlemail.com"].iter().any(|g| typed.contains(g));
            let microsoft =
                ["outlook.com", "office365.com", "office.com", "hotmail.", "live.com", "microsoft.com", "msn.com"].iter().any(|m| typed.contains(m));
            if google || microsoft {
                let (kind, server, who) = if google {
                    (SourceKind::Google, "apidata.googleusercontent.com", "Google")
                } else {
                    (SourceKind::Microsoft, "graph.microsoft.com", "Microsoft")
                };
                f.kind = kind;
                f.server = server.into();
                f.allow_insecure = false;
                f.password.clear();
                f.found.clear();
                f.home.clear();
                f.result =
                    Some(Ok(format!("That's {who}: it signs in with {who} rather than a password. Type your address and press Sign in with {who}.")));
            }
        }
        let provider = match f.kind {
            SourceKind::Google => Some(Provider::Google),
            SourceKind::Microsoft => Some(Provider::Microsoft),
            _ => None,
        };
        if provider.is_none() {
            row(ui, "Server", &mut |ui| {
                let hint = match f.kind {
                    SourceKind::Nextcloud => "cloud.example.com",
                    _ => "https://dav.example.com/ (or just the host name)",
                };
                ui.add_sized([300.0, 22.0], egui::TextEdit::singleline(&mut f.server).id(egui::Id::new("cal_src_server")).hint_text(hint));
            });
        }
        let mut provider_sign_in = false;
        if let Some(p) = provider {
            let (label, hint) = match p {
                Provider::Google => ("Google address", "you@gmail.com"),
                Provider::Microsoft => ("Microsoft address", "you@outlook.com"),
            };
            row(ui, label, &mut |ui| {
                ui.add_sized([300.0, 22.0], egui::TextEdit::singleline(&mut f.username).id(egui::Id::new("cal_src_user")).hint_text(hint));
            });
            let signed_in = crate::secrets::get(&format!("calendar:{}", f.id)).is_some();
            ui.horizontal(|ui| {
                ui.add_space(154.0);
                ui.add_enabled_ui(!f.busy && !f.signing_in, |ui| {
                    let label = if signed_in { format!("Sign in with {} again", p.name()) } else { format!("Sign in with {}", p.name()) };
                    if ui.button(label).clicked() {
                        provider_sign_in = true;
                    }
                });
                if f.signing_in {
                    ui.spinner();
                    ui.label(RichText::new("Finish signing in in your browser...").font(regular(11.5)).color(t.text_dim));
                } else if signed_in {
                    ui.label(RichText::new("Signed in.").font(regular(11.5)).color(t.green));
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(154.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(match p {
                            Provider::Google => "Uses MailCraft's Google app registration; its Google Cloud project needs the CalDAV API turned on.",
                            Provider::Microsoft => {
                                "Uses MailCraft's Microsoft app registration; Microsoft asks you to allow access to your calendars."
                            }
                        })
                        .font(regular(11.5))
                        .color(t.text_dim),
                    )
                    .wrap(),
                );
            });
        } else if f.kind != SourceKind::MailServer {
            row(ui, "Username", &mut |ui| {
                ui.add_sized([300.0, 22.0], egui::TextEdit::singleline(&mut f.username).id(egui::Id::new("cal_src_user")));
            });
            let saved = f.editing.is_some();
            row(ui, "Password", &mut |ui| {
                ui.add_sized(
                    [300.0, 22.0],
                    egui::TextEdit::singleline(&mut f.password).id(egui::Id::new("cal_src_pw")).password(true).hint_text(if saved {
                        "unchanged"
                    } else {
                        ""
                    }),
                );
            });
            if f.kind == SourceKind::Nextcloud {
                ui.horizontal(|ui| {
                    ui.add_space(154.0);
                    ui.label(
                        RichText::new("With two-factor sign-in, use an app password (Nextcloud: Settings > Security).")
                            .font(regular(11.5))
                            .color(t.text_dim),
                    );
                });
            }
        } else {
            ui.horizontal(|ui| {
                ui.add_space(154.0);
                ui.label(RichText::new("Signs in with the mail account's saved password.").font(regular(11.5)).color(t.text_dim));
            });
        }
        if provider.is_none() {
            ui.horizontal(|ui| {
                ui.add_space(154.0);
                ui.checkbox(&mut f.server_sends_invites, "This calendar server emails meeting invitations itself (MailCraft then doesn't)");
            });
            ui.horizontal(|ui| {
                ui.add_space(154.0);
                ui.checkbox(&mut f.allow_insecure, "Allow an unencrypted connection (test servers only)");
            });
        }
        match &f.result {
            Some(Ok(s)) => {
                ui.label(RichText::new(s).color(t.green));
            }
            Some(Err(e)) => {
                ui.add(egui::Label::new(RichText::new(e).color(t.red)).wrap());
            }
            None => {}
        }
        for c in &f.found {
            ui.horizontal(|ui| {
                ui.add_space(154.0);
                let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                let col = c.colour.as_deref().and_then(|h| {
                    let h = h.trim_start_matches('#');
                    let v = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
                    Some(Color32::from_rgb(v(0)?, v(2)?, v(4)?))
                });
                ui.painter().circle_filled(r.center(), 5.0, col.unwrap_or(t.accent));
                ui.label(if c.read_only { format!("{} (read-only)", c.name) } else { c.name.clone() });
            });
        }
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!f.busy && !f.server.trim().is_empty(), |ui| {
                if ui.button("Test").clicked() {
                    test = true;
                }
                if ui.add(egui::Button::new(RichText::new("Save").color(Color32::WHITE)).fill(t.accent)).clicked() {
                    save = true;
                }
            });
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
            if f.busy {
                ui.spinner();
            }
        });
        if cancel {
            // A new account that was signed in and then not kept: drop the sign-in.
            if let Some(f) = self.cal.form.take()
                && f.editing.is_none()
            {
                crate::secrets::forget(&format!("calendar:{}", f.id));
            }
            return;
        }
        let Some(f) = self.cal.form.as_mut() else { return };
        let src = CalSource {
            id: f.id.clone(),
            label: if f.label.trim().is_empty() {
                match f.kind {
                    SourceKind::MailServer => f.account.clone(),
                    SourceKind::Nextcloud => "Nextcloud".into(),
                    SourceKind::CalDav => {
                        f.server.trim().trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_string()
                    }
                    SourceKind::Google | SourceKind::Microsoft => {
                        if !f.username.trim().is_empty() {
                            f.username.trim().to_string()
                        } else if f.kind == SourceKind::Google {
                            "Google".into()
                        } else {
                            "Outlook".into()
                        }
                    }
                }
            } else {
                f.label.trim().to_string()
            },
            kind: f.kind,
            account: if f.kind == SourceKind::MailServer { f.account.clone() } else { String::new() },
            server: f.server.trim().to_string(),
            username: f.username.trim().to_string(),
            home: f.home.clone(),
            server_sends_invites: f.server_sends_invites,
            allow_insecure: f.allow_insecure,
        };
        let password = if !f.password.is_empty() { Some(f.password.clone()) } else { crate::secrets::get(&src.secret_name()) };
        if provider_sign_in {
            f.signing_in = true;
            f.result = None;
            let _ = self.worker.send(crate::worker::Request::CalSignIn(src));
            return;
        }
        let missing = match src.provider() {
            Some(p) => format!("Sign in with {} first.", p.name()),
            None => "Enter the password first.".to_string(),
        };
        if test {
            match password {
                Some(pw) => {
                    f.busy = true;
                    f.result = None;
                    let _ = self.worker.send(crate::worker::Request::Cal(CalRequest::Test(src, pw)));
                }
                None => f.result = Some(Err(missing.clone())),
            }
            return;
        }
        if save {
            if password.is_none() {
                f.result = Some(Err(missing));
                return;
            }
            if src.kind != SourceKind::MailServer && !f.password.is_empty() {
                crate::secrets::set(&src.secret_name(), &f.password);
            }
            // A changed server or user: find the calendars again.
            let mut src = src;
            if let Some(old) = self.cal.sources.iter().find(|s| s.id == src.id)
                && (old.server != src.server || old.username != src.username || old.kind != src.kind)
                && f.found.is_empty()
            {
                src.home.clear();
            }
            match self.cal.sources.iter_mut().find(|s| s.id == src.id) {
                Some(slot) => *slot = src.clone(),
                None => self.cal.sources.push(src.clone()),
            }
            self.cal.form = None;
            if let Err(e) = save_sources(&self.cal.sources) {
                self.status = format!("Could not save the calendar accounts: {e}");
            }
            self.cal.status.remove(&src.id);
            self.calendar_sync(&src);
        }
    }
}

/// One time choice every quarter hour (the time already set stays, if it is between).
fn time_combo(ui: &mut egui::Ui, id: &str, time: &mut NaiveTime) {
    egui::ComboBox::from_id_salt(id).width(80.0).selected_text(time.format("%H:%M").to_string()).height(300.0).show_ui(ui, |ui| {
        for q in 0..96u32 {
            if let Some(v) = NaiveTime::from_hms_opt(q / 4, (q % 4) * 15, 0) {
                ui.selectable_value(time, v, v.format("%H:%M").to_string());
            }
        }
    });
}

/// A small month to pick a day from: the month's name with back and on, a row of weekday
/// letters, and the days (today ringed, the chosen day filled, `marked` days dotted, the range
/// in view shaded).
fn mini_month(
    ui: &mut egui::Ui,
    t: &Tokens,
    month: &mut NaiveDate,
    chosen: Option<NaiveDate>,
    marked: &dyn Fn(NaiveDate) -> bool,
    range: Option<(NaiveDate, NaiveDate)>,
    picked: &mut Option<NaiveDate>,
) {
    let cell = 30.0;
    let w = cell * 7.0;
    let today = chrono::Local::now().date_naive();
    let id = ui.id().with(("mini", month.year(), month.month()));
    let (head, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
    let p = ui.painter().clone();
    p.text(head.left_center() + vec2(4.0, 0.0), egui::Align2::LEFT_CENTER, month.format("%B %Y").to_string(), semibold(13.0), t.text);
    for (k, (glyph, step)) in [("\u{2039}", -1), ("\u{203a}", 1)].into_iter().enumerate() {
        let r = Rect::from_center_size(pos2(head.right() - 44.0 + k as f32 * 26.0, head.center().y), vec2(24.0, 22.0));
        let resp = ui.interact(r, id.with(("step", k)), Sense::click());
        if resp.hovered() {
            p.rect_filled(r, 3.0, t.hover);
        }
        p.text(r.center(), egui::Align2::CENTER_CENTER, glyph, semibold(16.0), t.text);
        if resp.clicked() {
            *month = add_months(first_of_month(*month), step);
        }
    }
    let (letters, _) = ui.allocate_exact_size(vec2(w, 18.0), Sense::hover());
    for (i, l) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
        p.text(pos2(letters.left() + (i as f32 + 0.5) * cell, letters.center().y), egui::Align2::CENTER_CENTER, *l, regular(11.0), t.text_dim);
    }
    let first = week_start(first_of_month(*month));
    let (grid, _) = ui.allocate_exact_size(vec2(w, cell * 6.0), Sense::hover());
    for k in 0..42 {
        let d = first + Span::days(k);
        let r =
            Rect::from_min_size(pos2(grid.left() + (k % 7) as f32 * cell, grid.top() + (k / 7) as f32 * cell * 1.0), vec2(cell, cell)).shrink(1.0);
        let resp = ui.interact(r, id.with(("d", k)), Sense::click());
        let out = d.month() != month.month();
        if range.is_some_and(|(a, b)| d >= a && d < b) {
            p.rect_filled(r, 0.0, t.hover);
        }
        if chosen == Some(d) {
            p.circle_filled(r.center(), cell / 2.0 - 2.0, t.accent);
        } else if resp.hovered() {
            p.circle_filled(r.center(), cell / 2.0 - 2.0, t.pressed);
        }
        if d == today && chosen != Some(d) {
            p.circle_stroke(r.center(), cell / 2.0 - 2.0, Stroke::new(1.5, t.accent));
        }
        let ink = if chosen == Some(d) {
            Color32::WHITE
        } else if out {
            t.text_disabled
        } else {
            t.text
        };
        p.text(
            r.center() - vec2(0.0, 1.0),
            egui::Align2::CENTER_CENTER,
            d.day().to_string(),
            if d == today { semibold(12.0) } else { regular(12.0) },
            ink,
        );
        if marked(d) {
            p.circle_filled(r.center_bottom() - vec2(0.0, 4.0), 1.8, if chosen == Some(d) { Color32::WHITE } else { t.accent });
        }
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            *picked = Some(d);
            if out {
                *month = first_of_month(d);
            }
        }
    }
}

/// The calendar icon (16 x 16 at `r`): a page with two rings and a row of days, drawn here.
pub fn icon(p: &egui::Painter, r: Rect, c: Color32, accent: Color32) {
    let at = |x: f32, y: f32| pos2(r.left() + x * r.width() / 16.0, r.top() + y * r.height() / 16.0);
    let s = Stroke::new(1.3 * r.width() / 16.0, c);
    p.rect_stroke(Rect::from_min_max(at(1.5, 3.0), at(14.5, 14.5)), 1.5, s, egui::StrokeKind::Middle);
    p.rect_filled(Rect::from_min_max(at(1.5, 3.0), at(14.5, 6.0)), 1.0, accent);
    p.line_segment([at(5.0, 1.0), at(5.0, 4.5)], s);
    p.line_segment([at(11.0, 1.0), at(11.0, 4.5)], s);
    for (x, y) in [(4.5, 8.5), (8.0, 8.5), (11.5, 8.5), (4.5, 11.8), (8.0, 11.8)] {
        p.rect_filled(Rect::from_center_size(at(x, y), vec2(1.8, 1.8) * r.width() / 16.0), 0.3, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_choices_round_trip() {
        for r in [Repeat::None, Repeat::Daily, Repeat::Weekdays, Repeat::Weekly, Repeat::Monthly, Repeat::Yearly] {
            assert_eq!(Repeat::of(r.rule().as_deref()), r);
        }
        assert_eq!(Repeat::of(Some("FREQ=WEEKLY;INTERVAL=2")), Repeat::Custom("FREQ=WEEKLY;INTERVAL=2".into()));
        let d = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap_or_default();
        assert_eq!(Repeat::Monthly.label(d), "Every month on the 22nd");
        assert_eq!(ordinal(11), "11th");
        assert_eq!(ordinal(23), "23rd");
    }

    #[test]
    fn months_and_weeks() {
        let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap_or_default();
        assert_eq!(add_months(d(2026, 1, 31), 1), d(2026, 2, 28));
        assert_eq!(add_months(d(2026, 12, 9), 1), d(2027, 1, 9));
        assert_eq!(add_months(d(2026, 1, 9), -1), d(2025, 12, 9));
        assert_eq!(week_start(d(2026, 10, 9)), d(2026, 10, 5));
        assert_eq!(week_start(d(2026, 10, 5)), d(2026, 10, 5));
    }

    #[test]
    fn nextcloud_address() {
        let mut s = CalSource {
            id: "x".into(),
            label: String::new(),
            kind: SourceKind::Nextcloud,
            account: String::new(),
            server: "nc.example.org".into(),
            username: "m".into(),
            home: String::new(),
            allow_insecure: false,
            server_sends_invites: false,
        };
        assert_eq!(s.start_url(), "https://nc.example.org/remote.php/dav/");
        s.server = "https://nc.example.org/".into();
        assert_eq!(s.start_url(), "https://nc.example.org/remote.php/dav/");
        assert_eq!(s.secret_name(), "calendar:x");
        s.kind = SourceKind::MailServer;
        s.account = "me@example.org".into();
        assert_eq!(s.secret_name(), "me@example.org");
    }
}
