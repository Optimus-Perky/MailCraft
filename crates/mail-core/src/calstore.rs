//! Calendars kept locally (in the mail cache's database): each calendar of each calendar
//! account, and each event's iCalendar text with its etag, so the calendar opens at once and
//! only what changed is fetched again. Repeats are laid out when shown (ical::occurrences).

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::caldav::{Calendar, Client, Item, same_url};

/// A calendar server: CalDAV, or Microsoft Graph (which has no CalDAV).
pub enum Remote {
    CalDav(Client),
    Graph(crate::graph::Client),
}

impl Remote {
    /// Where the calendars are, from a start address (Graph needs none: it knows who signed in).
    pub fn discover(&self, start: &str) -> Result<String, Error> {
        match self {
            Remote::CalDav(c) => c.discover(start),
            Remote::Graph(_) => Ok(format!("{}me/", crate::graph::base())),
        }
    }

    pub fn calendars(&self, home: &str) -> Result<Vec<Calendar>, Error> {
        match self {
            Remote::CalDav(c) => c.calendars(home),
            Remote::Graph(g) => g.calendars(home),
        }
    }

    fn etags(&self, calendar: &str) -> Result<Vec<Item>, Error> {
        match self {
            Remote::CalDav(c) => c.etags(calendar),
            Remote::Graph(g) => g.etags(calendar),
        }
    }

    fn fetch(&self, calendar: &str, urls: &[String]) -> Result<Vec<Item>, Error> {
        match self {
            Remote::CalDav(c) => c.fetch(calendar, urls),
            Remote::Graph(g) => g.fetch(calendar, urls),
        }
    }

    /// Writes an event (to `url`, or as a new one in `calendar`); gives its address and etag.
    fn put(&self, calendar: &str, url: Option<&str>, uid: &str, ics: &str, etag: Option<&str>) -> Result<(String, Option<String>), Error> {
        match self {
            Remote::CalDav(c) => {
                let url = url.map(str::to_string).unwrap_or_else(|| crate::caldav::new_url(calendar, uid));
                let etag = c.put(&url, ics, etag)?;
                Ok((url, etag))
            }
            Remote::Graph(g) => g.put(calendar, url, ics),
        }
    }

    fn delete(&self, url: &str, etag: Option<&str>) -> Result<(), Error> {
        match self {
            Remote::CalDav(c) => c.delete(url, etag),
            Remote::Graph(g) => g.delete(url),
        }
    }

    /// Whether the server itself sends the invitations, replies and cancellations for this
    /// event: Microsoft does whenever it has (or, `before` a change, had) attendees; a CalDAV
    /// server when it says it schedules.
    pub fn schedules(&self, calendar: &str, e: &Event, before: Option<&Event>) -> bool {
        match self {
            Remote::CalDav(c) => c.auto_schedules(calendar),
            Remote::Graph(_) => !e.attendees.is_empty() || before.is_some_and(|b| !b.attendees.is_empty()),
        }
    }

    pub fn make_calendar(&self, home: &str, name: &str, colour: Option<&str>) -> Result<String, Error> {
        match self {
            Remote::CalDav(c) => c.make_calendar(home, name, colour),
            Remote::Graph(g) => g.make_calendar(home, name),
        }
    }
}
use crate::ical::{self, Event, Occurrence};
use crate::{Error, Store};

pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS calendar (
           id INTEGER PRIMARY KEY, source TEXT NOT NULL, url TEXT NOT NULL, name TEXT NOT NULL,
           colour TEXT, own_colour TEXT, ctag TEXT, read_only INTEGER NOT NULL DEFAULT 0,
           shown INTEGER NOT NULL DEFAULT 1, UNIQUE(source, url));
         CREATE TABLE IF NOT EXISTS event (
           id INTEGER PRIMARY KEY, calendar_id INTEGER NOT NULL REFERENCES calendar(id) ON DELETE CASCADE,
           url TEXT NOT NULL, etag TEXT NOT NULL, uid TEXT NOT NULL, ics TEXT NOT NULL,
           first TEXT NOT NULL, last TEXT, UNIQUE(calendar_id, url));
         CREATE INDEX IF NOT EXISTS event_by_time ON event(first, last);",
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalRow {
    pub id: i64,
    /// Which calendar account it belongs to.
    pub source: String,
    pub url: String,
    pub name: String,
    /// The colour chosen here, else the server's.
    pub colour: Option<String>,
    pub ctag: Option<String>,
    pub read_only: bool,
    pub shown: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub id: i64,
    pub calendar_id: i64,
    pub url: String,
    pub etag: String,
    pub ics: String,
}

/// One thing to draw: which stored event, the event (a changed instance is its own), and when.
#[derive(Debug, Clone)]
pub struct Shown {
    pub row: i64,
    pub calendar_id: i64,
    pub event: Event,
    pub at: Occurrence,
}

/// When an event's text can show: its first start, and its last end (None: repeats for ever).
fn span(ics: &str) -> (String, Option<String>) {
    let tz = rrule::Tz::UTC;
    let evs = ical::parse(ics);
    let first = evs.iter().map(|e| e.start.utc(tz) - chrono::Duration::days(1)).min().unwrap_or_else(Utc::now);
    let repeats = evs.iter().any(|e| e.rrule.is_some() && e.recurrence_id.is_none());
    // A day either side covers all-day and floating times in any zone.
    let last = if repeats { None } else { evs.iter().map(|e| e.end.utc(tz) + chrono::Duration::days(1)).max().map(|t| t.to_rfc3339()) };
    (first.to_rfc3339(), last)
}

fn cal_row(r: &rusqlite::Row) -> rusqlite::Result<CalRow> {
    let own: Option<String> = r.get(4)?;
    let server: Option<String> = r.get(5)?;
    Ok(CalRow {
        id: r.get(0)?,
        source: r.get(1)?,
        url: r.get(2)?,
        name: r.get(3)?,
        colour: own.or(server),
        ctag: r.get(6)?,
        read_only: r.get::<_, i64>(7)? != 0,
        shown: r.get::<_, i64>(8)? != 0,
    })
}

const CAL_COLS: &str = "id, source, url, name, own_colour, colour, ctag, read_only, shown";

impl Store {
    pub fn calendars(&self) -> rusqlite::Result<Vec<CalRow>> {
        let mut q = self.db.prepare(&format!("SELECT {CAL_COLS} FROM calendar ORDER BY source, name COLLATE NOCASE"))?;
        q.query_map([], cal_row)?.collect()
    }

    pub fn calendar(&self, id: i64) -> rusqlite::Result<Option<CalRow>> {
        self.db.query_row(&format!("SELECT {CAL_COLS} FROM calendar WHERE id = ?1"), [id], cal_row).optional()
    }

    /// Adds or updates a calendar as the server describes it (keeping what was chosen here, and
    /// the ctag, which moves only once its events are in).
    pub fn put_calendar(&self, source: &str, c: &Calendar) -> rusqlite::Result<i64> {
        self.db.execute(
            "INSERT INTO calendar(source, url, name, colour, read_only) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(source, url) DO UPDATE SET name = excluded.name, colour = excluded.colour, read_only = excluded.read_only",
            params![source, c.url, c.name, c.colour, c.read_only as i64],
        )?;
        self.db.query_row("SELECT id FROM calendar WHERE source = ?1 AND url = ?2", params![source, c.url], |r| r.get(0))
    }

    /// Forgets a source's calendars that are no longer on the server (all of them: `keep` empty).
    pub fn prune_calendars(&self, source: &str, keep: &[String]) -> rusqlite::Result<usize> {
        let ids: Vec<(i64, String)> = self
            .db
            .prepare("SELECT id, url FROM calendar WHERE source = ?1")?
            .query_map([source], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut n = 0;
        for (id, url) in ids {
            if !keep.contains(&url) {
                n += self.db.execute("DELETE FROM calendar WHERE id = ?1", [id])?;
            }
        }
        Ok(n)
    }

    pub fn set_calendar_ctag(&self, id: i64, ctag: Option<&str>) -> rusqlite::Result<()> {
        self.db.execute("UPDATE calendar SET ctag = ?2 WHERE id = ?1", params![id, ctag]).map(|_| ())
    }

    pub fn set_calendar_shown(&self, id: i64, shown: bool) -> rusqlite::Result<()> {
        self.db.execute("UPDATE calendar SET shown = ?2 WHERE id = ?1", params![id, shown as i64]).map(|_| ())
    }

    /// The colour chosen here (None: the server's again).
    pub fn set_calendar_colour(&self, id: i64, colour: Option<&str>) -> rusqlite::Result<()> {
        self.db.execute("UPDATE calendar SET own_colour = ?2 WHERE id = ?1", params![id, colour]).map(|_| ())
    }

    pub fn event_etags(&self, calendar_id: i64) -> rusqlite::Result<Vec<(String, String)>> {
        let mut q = self.db.prepare("SELECT url, etag FROM event WHERE calendar_id = ?1")?;
        q.query_map([calendar_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
    }

    pub fn put_event(&self, calendar_id: i64, url: &str, etag: &str, ics: &str) -> rusqlite::Result<i64> {
        let uid = ical::parse(ics).first().map(|e| e.uid.clone()).unwrap_or_default();
        let (first, last) = span(ics);
        self.db.execute(
            "INSERT INTO event(calendar_id, url, etag, uid, ics, first, last) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(calendar_id, url) DO UPDATE SET etag = excluded.etag, uid = excluded.uid, ics = excluded.ics,
               first = excluded.first, last = excluded.last",
            params![calendar_id, url, etag, uid, ics, first, last],
        )?;
        self.db.query_row("SELECT id FROM event WHERE calendar_id = ?1 AND url = ?2", params![calendar_id, url], |r| r.get(0))
    }

    pub fn remove_events(&self, calendar_id: i64, urls: &[String]) -> rusqlite::Result<()> {
        for u in urls {
            self.db.execute("DELETE FROM event WHERE calendar_id = ?1 AND url = ?2", params![calendar_id, u])?;
        }
        Ok(())
    }

    pub fn event_row(&self, id: i64) -> rusqlite::Result<Option<EventRow>> {
        self.db
            .query_row("SELECT id, calendar_id, url, etag, ics FROM event WHERE id = ?1", [id], |r| {
                Ok(EventRow { id: r.get(0)?, calendar_id: r.get(1)?, url: r.get(2)?, etag: r.get(3)?, ics: r.get(4)? })
            })
            .optional()
    }

    /// The stored events whose UID is `uid` (a meeting already in a calendar), the main event
    /// first. Long lines are folded in the stored text, so it is unfolded to look.
    pub fn events_with_uid(&self, uid: &str) -> rusqlite::Result<Vec<EventRow>> {
        let mut q = self.db.prepare(
            "SELECT id, calendar_id, url, etag, ics FROM event
             WHERE instr(replace(replace(ics, char(13) || char(10) || ' ', ''), char(10) || ' ', ''), 'UID:' || ?1) > 0",
        )?;
        let rows: Vec<EventRow> = q
            .query_map([uid], |r| Ok(EventRow { id: r.get(0)?, calendar_id: r.get(1)?, url: r.get(2)?, etag: r.get(3)?, ics: r.get(4)? }))?
            .collect::<Result<_, _>>()?;
        // The text matched; keep only those whose event really has that UID.
        Ok(rows.into_iter().filter(|r| ical::parse(&r.ics).iter().any(|e| e.uid == uid)).collect())
    }

    /// The stored events (of shown calendars) that may happen between `from` and `to`.
    pub fn events_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> rusqlite::Result<Vec<EventRow>> {
        let mut q = self.db.prepare(
            "SELECT e.id, e.calendar_id, e.url, e.etag, e.ics FROM event e JOIN calendar c ON c.id = e.calendar_id
             WHERE c.shown = 1 AND e.first < ?2 AND (e.last IS NULL OR e.last > ?1)",
        )?;
        q.query_map(params![from.to_rfc3339(), to.to_rfc3339()], |r| {
            Ok(EventRow { id: r.get(0)?, calendar_id: r.get(1)?, url: r.get(2)?, etag: r.get(3)?, ics: r.get(4)? })
        })?
        .collect()
    }

    /// Everything to show between `from` and `to`, repeats laid out, earliest first.
    pub fn shown_between(&self, from: DateTime<Utc>, to: DateTime<Utc>, local: rrule::Tz) -> rusqlite::Result<Vec<Shown>> {
        Ok(lay_out(&self.events_between(from, to)?, from, to, local))
    }
}

/// The occurrences of stored events: a repeating event's changed instances replace the
/// instances they stand for, and cancelled ones are left out.
pub fn lay_out(rows: &[EventRow], from: DateTime<Utc>, to: DateTime<Utc>, local: rrule::Tz) -> Vec<Shown> {
    let mut out = vec![];
    for row in rows {
        let evs = ical::parse(&row.ics);
        let changed: Vec<DateTime<Utc>> = evs.iter().filter_map(|e| e.recurrence_id.map(|r| r.utc(local))).collect();
        for e in &evs {
            if e.status == "CANCELLED" {
                continue;
            }
            for at in ical::occurrences(e, from, to, local, if e.recurrence_id.is_some() { &[] } else { &changed }) {
                let at = match e.recurrence_id {
                    Some(r) => Occurrence { instance: Some(r.utc(local)), ..at },
                    None => at,
                };
                out.push(Shown { row: row.id, calendar_id: row.calendar_id, event: e.clone(), at });
            }
        }
    }
    out.sort_by_key(|a| (!a.event.start.is_date(), a.at.start));
    out
}

/// What a sync did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub calendars: usize,
    pub fetched: usize,
    pub removed: usize,
}

/// Brings one calendar account's calendars up to date: the list, then (for each whose ctag
/// moved) the events that are new or changed, and those gone.
pub fn sync(store: &Store, client: &Remote, source: &str, home: &str) -> Result<SyncReport, Error> {
    let cals = client.calendars(home)?;
    store.prune_calendars(source, &cals.iter().map(|c| c.url.clone()).collect::<Vec<_>>())?;
    let mut report = SyncReport { calendars: cals.len(), ..Default::default() };
    for cal in &cals {
        let before = store.calendars()?.into_iter().find(|c| c.source == source && c.url == cal.url).and_then(|c| c.ctag);
        let id = store.put_calendar(source, cal)?;
        if cal.ctag.is_some() && before == cal.ctag {
            continue;
        }
        let remote = client.etags(&cal.url)?;
        let local = store.event_etags(id)?;
        let wanted: Vec<String> =
            remote.iter().filter(|r| !local.iter().any(|(u, e)| same_url(u, &r.url) && *e == r.etag)).map(|r| r.url.clone()).collect();
        let gone: Vec<String> = local.iter().filter(|(u, _)| !remote.iter().any(|r| same_url(&r.url, u))).map(|(u, _)| u.clone()).collect();
        for item in client.fetch(&cal.url, &wanted)? {
            // Kept under the address it was stored as, however the server spells it now.
            let url = local.iter().find(|(u, _)| same_url(u, &item.url)).map(|(u, _)| u.clone()).unwrap_or(item.url);
            store.put_event(id, &url, &item.etag, &item.data)?;
            report.fetched += 1;
        }
        store.remove_events(id, &gone)?;
        report.removed += gone.len();
        store.set_calendar_ctag(id, cal.ctag.as_deref())?;
    }
    Ok(report)
}

/// Saves an event to its calendar's server and here. `row` is the stored event being changed
/// (None: a new one). The event's own changed instances, if any, go with it.
pub fn save(store: &Store, client: &Remote, calendar_id: i64, row: Option<i64>, e: &Event) -> Result<i64, Error> {
    let cal = store.calendar(calendar_id)?.ok_or_else(|| Error::Send("that calendar is no longer here".into()))?;
    if cal.read_only {
        return Err(Error::Send(format!("{} is read-only", cal.name)));
    }
    let old = match row {
        Some(r) => store.event_row(r)?,
        None => None,
    };
    let extra: Vec<Event> = old.as_ref().map(|o| ical::parse(&o.ics).into_iter().filter(|x| x.recurrence_id.is_some()).collect()).unwrap_or_default();
    let ics = ical::to_ics(e, &extra);
    let (url, etag) = client.put(&cal.url, old.as_ref().map(|o| o.url.as_str()), &e.uid, &ics, old.as_ref().map(|o| o.etag.as_str()))?;
    // Without an etag in the reply, ask for it (the next sync would fetch the event again otherwise).
    let etag = match etag {
        Some(t) => t,
        None => client.etags(&cal.url)?.into_iter().find(|i| same_url(&i.url, &url)).map(|i| i.etag).unwrap_or_default(),
    };
    Ok(store.put_event(calendar_id, &url, &etag, &ics)?)
}

/// Deletes a stored event from its server and here.
pub fn delete(store: &Store, client: &Remote, row: i64) -> Result<(), Error> {
    let Some(old) = store.event_row(row)? else { return Ok(()) };
    client.delete(&old.url, Some(&old.etag))?;
    store.remove_events(old.calendar_id, &[old.url])?;
    Ok(())
}

/// A fresh unique id for a new event.
pub fn new_uid() -> String {
    let mut b = [0u8; 12];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b);
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{hex}@mailcraft")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ical::When;
    use chrono::TimeZone;

    #[test]
    fn stores_and_lays_out() {
        let dir = std::env::temp_dir().join(format!("mc-cal-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let s = Store::open(&dir).unwrap();
        let cal = Calendar {
            url: "https://h/c/home/".into(),
            name: "Home".into(),
            colour: Some("#2D4A8A".into()),
            ctag: Some("1".into()),
            read_only: false,
        };
        let id = s.put_calendar("src", &cal).unwrap();
        assert_eq!(s.put_calendar("src", &Calendar { name: "Home 2".into(), ..cal.clone() }).unwrap(), id);
        s.set_calendar_colour(id, Some("#AA0000")).unwrap();
        assert_eq!(s.calendar(id).unwrap().map(|c| (c.name, c.colour)), Some(("Home 2".into(), Some("#AA0000".into()))));
        // A weekly event with its third instance moved an hour later.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:w@x\r\nSUMMARY:Gym\r\nDTSTART:20261005T180000Z\r\nDTEND:20261005T190000Z\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:w@x\r\nRECURRENCE-ID:20261019T180000Z\r\nSUMMARY:Gym (late)\r\nDTSTART:20261019T190000Z\r\nDTEND:20261019T200000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        s.put_event(id, "https://h/c/home/w.ics", "\"1\"", ics).unwrap();
        s.put_event(
            id,
            "https://h/c/home/old.ics",
            "\"1\"",
            "BEGIN:VEVENT\r\nUID:o\r\nDTSTART:20200101T100000Z\r\nDTEND:20200101T110000Z\r\nEND:VEVENT\r\n",
        )
        .unwrap();
        let from = Utc.with_ymd_and_hms(2026, 10, 12, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 10, 26, 0, 0, 0).unwrap();
        assert_eq!(s.events_between(from, to).unwrap().len(), 1, "the 2020 one is out of range");
        let shown = s.shown_between(from, to, rrule::Tz::UTC).unwrap();
        let what: Vec<(String, u32)> = shown.iter().map(|x| (x.event.summary.clone(), chrono::Timelike::hour(&x.at.start))).collect();
        assert_eq!(what, vec![("Gym".into(), 18), ("Gym (late)".into(), 19)]);
        s.set_calendar_shown(id, false).unwrap();
        assert!(s.events_between(from, to).unwrap().is_empty());
        s.prune_calendars("src", &[]).unwrap();
        assert!(s.calendars().unwrap().is_empty());
        assert!(s.event_etags(id).unwrap().is_empty(), "events go with their calendar");
        let _ = std::fs::remove_file(&dir);
    }

    /// Sync, save and delete against a real server (see caldav::tests::live_caldav).
    #[test]
    #[ignore]
    fn live_sync() {
        let Ok(server) = std::env::var("MAILCRAFT_CALDAV_TEST") else { return };
        let dir = std::env::temp_dir().join(format!("mc-live-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let s = Store::open(&dir).unwrap();
        let mut c = Client::new(crate::caldav::Auth::Basic { user: "test".into(), pass: "testpass".into() });
        c.insecure_ok = true;
        let home = c.discover(&server).unwrap();
        let c = Remote::CalDav(c);
        let first = sync(&s, &c, "radicale", &home).unwrap();
        assert!(first.calendars >= 1);
        let cal = s.calendars().unwrap().remove(0);
        let start = Utc.with_ymd_and_hms(2026, 11, 2, 9, 0, 0).unwrap();
        let mut e = Event {
            uid: new_uid(),
            summary: "Dentist".into(),
            start: When::Time(start),
            end: When::Time(start + chrono::Duration::minutes(30)),
            alarms: vec![60],
            ..Default::default()
        };
        let row = save(&s, &c, cal.id, None, &e).unwrap();
        // Nothing new to fetch: what was saved is already here with the server's etag.
        assert_eq!(sync(&s, &c, "radicale", &home).unwrap().fetched, 0);
        e.summary = "Dentist (moved)".into();
        let row = save(&s, &c, cal.id, Some(row), &e).unwrap();
        // A second copy of the store, synced fresh, sees the change.
        let dir2 = dir.with_extension("2.db");
        let _ = std::fs::remove_file(&dir2);
        let s2 = Store::open(&dir2).unwrap();
        sync(&s2, &c, "radicale", &home).unwrap();
        let day = s2.shown_between(start - chrono::Duration::hours(1), start + chrono::Duration::hours(1), rrule::Tz::UTC).unwrap();
        assert!(day.iter().any(|x| x.event.summary == "Dentist (moved)" && x.event.alarms == vec![60]), "{day:?}");
        delete(&s, &c, row).unwrap();
        assert_eq!(sync(&s2, &c, "radicale", &home).unwrap().removed, 1);
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(&dir2);
    }
}
