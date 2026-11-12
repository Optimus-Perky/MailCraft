//! Microsoft calendars (Outlook.com, Hotmail, Microsoft 365) through Microsoft Graph: Microsoft
//! has no CalDAV, so this speaks Graph's REST API and hands the rest of MailCraft the same things
//! a CalDAV server would (calendars, events as iCalendar text, a version tag per event).
//!
//! Events are read from each calendar's calendarView: every occurrence over a window, with
//! Outlook's own changes to single occurrences of a repeating meeting (moved, cancelled) already
//! applied. Each occurrence is kept as an event of its own, so changing or deleting one from
//! MailCraft changes that occurrence only. A new repeating event is written as a series (its
//! repeat rule turned into Graph's pattern); after the next sync its occurrences arrive.
//!
//! Times are asked for in UTC (`Prefer: outlook.timezone="UTC"`) and notes as plain text.
//! The version tag is Outlook's changeKey.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Datelike, Duration as Span, NaiveDate, NaiveDateTime, Utc, Weekday};
use serde_json::{Value, json};
use ureq::http;

use crate::Error;
use crate::caldav::{Calendar, Item};
use crate::ical::{self, Attendee, Event, When};

const GRAPH: &str = "https://graph.microsoft.com/v1.0/";

/// Graph's address (automated tests point MAILCRAFT_TEST_GRAPH at a stand-in).
pub fn base() -> String {
    std::env::var("MAILCRAFT_TEST_GRAPH").ok().filter(|b| !b.is_empty()).unwrap_or_else(|| GRAPH.to_string())
}

/// How far back and ahead occurrences are kept.
const BACK_DAYS: i64 = 365;
const AHEAD_DAYS: i64 = 730;

pub struct Client {
    agent: ureq::Agent,
    token: String,
    /// The last listing's events, so `fetch` needs no second round of requests.
    listed: Mutex<HashMap<String, Item>>,
}

impl Client {
    pub fn new(access_token: &str) -> Client {
        let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).http_status_as_error(false).build().new_agent();
        Client { agent, token: access_token.to_string(), listed: Mutex::new(HashMap::new()) }
    }

    fn call(&self, method: &str, url: &str, body: Option<&Value>) -> Result<Value, Error> {
        let m = http::Method::from_bytes(method.as_bytes()).map_err(|e| Error::Calendar(format!("{method}: {e}")))?;
        let mut b = http::Request::builder()
            .method(m)
            .uri(url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Prefer", "outlook.timezone=\"UTC\", outlook.body-content-type=\"text\"")
            .header("Accept", "application/json");
        if body.is_some() {
            b = b.header("Content-Type", "application/json");
        }
        let req = b.body(body.map(Value::to_string).unwrap_or_default()).map_err(|e| Error::Calendar(format!("{url}: {e}")))?;
        let mut resp = self.agent.run(req).map_err(|e| Error::Connect(url.to_string(), e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().with_config().limit(64 << 20).read_to_string().unwrap_or_default();
        let v: Value = if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) };
        if (200..300).contains(&status) {
            return Ok(v);
        }
        let why = v.pointer("/error/message").and_then(Value::as_str).unwrap_or("").chars().take(300).collect::<String>();
        let why = if why.is_empty() { String::new() } else { format!(": {why}") };
        Err(match status {
            401 | 403 => Error::Login(format!("Microsoft Graph answered {status}{why}")),
            _ => Error::Calendar(format!("Microsoft Graph answered {status}{why}")),
        })
    }

    /// Every page of a listing.
    fn all(&self, first: &str) -> Result<Vec<Value>, Error> {
        let mut out = vec![];
        let mut next = Some(first.to_string());
        let mut pages = 0;
        while let Some(url) = next.take() {
            let v = self.call("GET", &url, None)?;
            if let Some(items) = v.get("value").and_then(Value::as_array) {
                out.extend(items.iter().cloned());
            }
            next = v.get("@odata.nextLink").and_then(Value::as_str).map(str::to_string);
            pages += 1;
            if pages > 200 {
                break;
            }
        }
        Ok(out)
    }

    /// The person's calendars. `home` is not needed (Graph knows who is signed in).
    pub fn calendars(&self, _home: &str) -> Result<Vec<Calendar>, Error> {
        let list = self.all(&format!("{}me/calendars?$select=id,name,hexColor,color,canEdit&$top=100", base()))?;
        Ok(list
            .iter()
            .filter_map(|c| {
                let id = c.get("id")?.as_str()?;
                let colour = c
                    .get("hexColor")
                    .and_then(Value::as_str)
                    .filter(|h| h.len() >= 7)
                    .map(str::to_string)
                    .or_else(|| c.get("color").and_then(Value::as_str).and_then(colour_for).map(str::to_string));
                Some(Calendar {
                    url: format!("{}me/calendars/{}/", base(), enc(id)),
                    name: c.get("name").and_then(Value::as_str).unwrap_or("Calendar").to_string(),
                    colour,
                    ctag: None,
                    read_only: !c.get("canEdit").and_then(Value::as_bool).unwrap_or(true),
                })
            })
            .collect())
    }

    /// Every occurrence in the window, with its version tag (and its text, kept for `fetch`).
    pub fn etags(&self, calendar: &str) -> Result<Vec<Item>, Error> {
        let now = Utc::now();
        let from = (now - Span::days(BACK_DAYS)).format("%Y-%m-%dT00:00:00Z");
        let to = (now + Span::days(AHEAD_DAYS)).format("%Y-%m-%dT00:00:00Z");
        let url = format!(
            "{}calendarView?startDateTime={from}&endDateTime={to}&$top=500&$select={}",
            calendar.trim_end_matches('/').to_string() + "/",
            FIELDS
        );
        let mut items = vec![];
        let mut listed = HashMap::new();
        for ev in self.all(&url)? {
            let Some(id) = ev.get("id").and_then(Value::as_str) else { continue };
            if ev.get("isCancelled").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let item = Item {
                url: format!("{}me/events/{}", base(), enc(id)),
                etag: ev.get("changeKey").and_then(Value::as_str).unwrap_or("").to_string(),
                data: ical::to_ics(&from_graph(&ev), &[]),
            };
            listed.insert(item.url.clone(), item.clone());
            items.push(Item { data: String::new(), ..item });
        }
        if let Ok(mut l) = self.listed.lock() {
            *l = listed;
        }
        Ok(items)
    }

    /// The events at `urls` (from the last listing; any not in it are asked for).
    pub fn fetch(&self, _calendar: &str, urls: &[String]) -> Result<Vec<Item>, Error> {
        let mut out = vec![];
        for url in urls {
            let got = self.listed.lock().ok().and_then(|l| l.get(url).cloned());
            match got {
                Some(i) => out.push(i),
                None => {
                    let ev = self.call("GET", &format!("{url}?$select={FIELDS}"), None)?;
                    out.push(Item {
                        url: url.clone(),
                        etag: ev.get("changeKey").and_then(Value::as_str).unwrap_or("").to_string(),
                        data: ical::to_ics(&from_graph(&ev), &[]),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Writes an event: changes the one at `url`, or makes a new one in `calendar`. Gives its
    /// address and version tag.
    pub fn put(&self, calendar: &str, url: Option<&str>, ics: &str) -> Result<(String, Option<String>), Error> {
        let e = ical::parse(ics).into_iter().find(|e| e.recurrence_id.is_none()).ok_or_else(|| Error::Calendar("no event to save".into()))?;
        let new = url.is_none();
        let mut body = to_graph(&e, new);
        // A change: the guest list is only Outlook's to change when I organise the meeting. Then
        // it goes as it now is, even empty (Outlook tells those taken off); otherwise not at all.
        if let Some(u) = url {
            let organiser = self.call("GET", &format!("{u}?$select=isOrganizer"), None)?.get("isOrganizer").and_then(Value::as_bool).unwrap_or(false);
            if let Some(obj) = body.as_object_mut() {
                if organiser {
                    obj.insert("attendees".into(), attendees_json(&e));
                } else {
                    obj.remove("attendees");
                }
            }
        }
        let v = match url {
            Some(u) => self.call("PATCH", u, Some(&body))?,
            None => self.call("POST", &format!("{}events", calendar.trim_end_matches('/').to_string() + "/"), Some(&body))?,
        };
        let id = v.get("id").and_then(Value::as_str).ok_or_else(|| Error::Calendar("Microsoft Graph gave no id for the event".into()))?;
        Ok((format!("{}me/events/{}", base(), enc(id)), v.get("changeKey").and_then(Value::as_str).map(str::to_string)))
    }

    pub fn delete(&self, url: &str) -> Result<(), Error> {
        match self.call("DELETE", url, None) {
            Ok(_) => Ok(()),
            // Already gone.
            Err(Error::Calendar(m)) if m.contains(" 404") => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn make_calendar(&self, _home: &str, name: &str) -> Result<String, Error> {
        let v = self.call("POST", &format!("{}me/calendars", base()), Some(&json!({ "name": name })))?;
        let id = v.get("id").and_then(Value::as_str).ok_or_else(|| Error::Calendar("Microsoft Graph gave no id for the calendar".into()))?;
        Ok(format!("{}me/calendars/{}/", base(), enc(id)))
    }
}

const FIELDS: &str = "id,iCalUId,subject,body,start,end,isAllDay,location,recurrence,attendees,organizer,isReminderOn,reminderMinutesBeforeStart,showAs,isCancelled,changeKey,type,seriesMasterId,responseStatus";

/// Percent-encodes the characters Graph ids can carry that a path cannot.
fn enc(id: &str) -> String {
    let mut s = String::with_capacity(id.len());
    for c in id.chars() {
        match c {
            '/' => s.push_str("%2F"),
            '+' => s.push_str("%2B"),
            '?' => s.push_str("%3F"),
            '#' => s.push_str("%23"),
            ' ' => s.push_str("%20"),
            _ => s.push(c),
        }
    }
    s
}

/// Outlook's named calendar colours, roughly.
fn colour_for(name: &str) -> Option<&'static str> {
    Some(match name {
        "lightBlue" => "#5B9BD5",
        "lightGreen" => "#70AD47",
        "lightOrange" => "#ED7D31",
        "lightGray" => "#A5A5A5",
        "lightYellow" => "#FFC000",
        "lightTeal" => "#4BACC6",
        "lightPink" => "#E46C9B",
        "lightBrown" => "#A47D5C",
        "lightRed" => "#E74C3C",
        _ => return None,
    })
}

fn text<'a>(v: &'a Value, path: &str) -> &'a str {
    v.pointer(path).and_then(Value::as_str).unwrap_or("")
}

/// Graph's "2026-10-10T09:00:00.0000000" (UTC, as asked for).
fn graph_time(s: &str) -> Option<NaiveDateTime> {
    let s = s.split('.').next().unwrap_or(s);
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok()
}

/// A Graph event as MailCraft's event.
pub fn from_graph(v: &Value) -> Event {
    let all_day = v.get("isAllDay").and_then(Value::as_bool).unwrap_or(false);
    let when = |path: &str| -> When {
        let t = graph_time(text(v, path)).unwrap_or_else(|| Utc::now().naive_utc());
        if all_day { When::Date(t.date()) } else { When::Time(DateTime::from_naive_utc_and_offset(t, Utc)) }
    };
    let person = |p: &Value| Attendee {
        email: text(p, "/emailAddress/address").to_string(),
        name: text(p, "/emailAddress/name").to_string(),
        partstat: match text(p, "/status/response") {
            "accepted" | "organizer" => "ACCEPTED",
            "declined" => "DECLINED",
            "tentativelyAccepted" => "TENTATIVE",
            _ => "NEEDS-ACTION",
        }
        .to_string(),
    };
    let uid = Some(text(v, "/iCalUId")).filter(|s| !s.is_empty()).unwrap_or(text(v, "/id")).to_string();
    // An occurrence of a series is kept on its own: its own uid, so it never merges with the series.
    let uid = if text(v, "/type") == "occurrence" || text(v, "/type") == "exception" {
        format!("{uid}-{}", text(v, "/start/dateTime").split('.').next().unwrap_or("").replace([':', '-'], ""))
    } else {
        uid
    };
    let mut e = Event {
        uid,
        summary: text(v, "/subject").to_string(),
        location: text(v, "/location/displayName").to_string(),
        description: text(v, "/body/content").trim().to_string(),
        start: when("/start/dateTime"),
        end: when("/end/dateTime"),
        tzid: None,
        rrule: None,
        alarms: vec![],
        organizer: v.get("organizer").map(person).filter(|a| !a.email.is_empty()),
        attendees: v.get("attendees").and_then(Value::as_array).map(|a| a.iter().map(person).collect()).unwrap_or_default(),
        status: if v.get("isCancelled").and_then(Value::as_bool) == Some(true) { "CANCELLED" } else { "CONFIRMED" }.to_string(),
        transparent: text(v, "/showAs") == "free",
        ..Event::default()
    };
    if v.get("isReminderOn").and_then(Value::as_bool) == Some(true) {
        e.alarms.push(v.get("reminderMinutesBeforeStart").and_then(Value::as_i64).unwrap_or(15));
    }
    // A series master (only met when one is fetched by itself, e.g. just after it was made).
    if text(v, "/type") == "seriesMaster" {
        e.rrule = v.get("recurrence").and_then(rrule_from_graph);
    }
    e
}

/// MailCraft's event as a Graph event body. Attendees go whenever there are any (Outlook then
/// sends the invitations or updates); the repeat pattern only with a new event (`new`): an
/// existing series is changed in Outlook.
pub fn to_graph(e: &Event, new: bool) -> Value {
    let all_day = e.start.is_date();
    let time = |w: When, end: bool| -> Value {
        let t = match w {
            When::Date(d) => d.and_hms_opt(0, 0, 0).unwrap_or_default(),
            When::Time(t) => t.naive_utc(),
            When::Floating(n) => n,
        };
        // An all-day end must be midnight after the last day.
        let t = if all_day && end && t <= e.start.utc(ical::local_zone()).naive_utc() { t + Span::days(1) } else { t };
        json!({ "dateTime": t.format("%Y-%m-%dT%H:%M:%S").to_string(), "timeZone": "UTC" })
    };
    let mut v = json!({
        "subject": e.summary,
        "body": { "contentType": "text", "content": e.description },
        "start": time(e.start, false),
        "end": time(e.end, true),
        "isAllDay": all_day,
        "location": { "displayName": e.location },
        "showAs": if e.transparent { "free" } else { "busy" },
        "isReminderOn": !e.alarms.is_empty(),
    });
    if let Some(m) = e.alarms.first() {
        v["reminderMinutesBeforeStart"] = json!(m);
    }
    if !e.attendees.is_empty() {
        v["attendees"] = attendees_json(e);
    }
    if new && let Some(r) = e.rrule.as_deref().and_then(|r| rrule_to_graph(r, e.start.utc(ical::local_zone()).date_naive())) {
        v["recurrence"] = r;
    }
    v
}

fn attendees_json(e: &Event) -> Value {
    Value::Array(e.attendees.iter().map(|a| json!({ "emailAddress": { "address": a.email, "name": a.name }, "type": "required" })).collect())
}

const DAYS: [(&str, &str, Weekday); 7] = [
    ("MO", "monday", Weekday::Mon),
    ("TU", "tuesday", Weekday::Tue),
    ("WE", "wednesday", Weekday::Wed),
    ("TH", "thursday", Weekday::Thu),
    ("FR", "friday", Weekday::Fri),
    ("SA", "saturday", Weekday::Sat),
    ("SU", "sunday", Weekday::Sun),
];

/// Graph's repeat pattern as an RRULE.
pub fn rrule_from_graph(r: &Value) -> Option<String> {
    let p = r.get("pattern")?;
    let interval = p.get("interval").and_then(Value::as_i64).unwrap_or(1).max(1);
    let days: Vec<&str> = p
        .get("daysOfWeek")
        .and_then(Value::as_array)
        .map(|d| d.iter().filter_map(Value::as_str).filter_map(|n| DAYS.iter().find(|x| x.1 == n).map(|x| x.0)).collect())
        .unwrap_or_default();
    let setpos = match p.get("index").and_then(Value::as_str).unwrap_or("first") {
        "second" => 2,
        "third" => 3,
        "fourth" => 4,
        "last" => -1,
        _ => 1,
    };
    let month = p.get("month").and_then(Value::as_i64).unwrap_or(0);
    let mday = p.get("dayOfMonth").and_then(Value::as_i64).unwrap_or(0);
    let mut rule = match p.get("type").and_then(Value::as_str)? {
        "daily" => "FREQ=DAILY".to_string(),
        "weekly" => format!("FREQ=WEEKLY;BYDAY={}", days.join(",")),
        "absoluteMonthly" => format!("FREQ=MONTHLY;BYMONTHDAY={mday}"),
        "relativeMonthly" => format!("FREQ=MONTHLY;BYDAY={};BYSETPOS={setpos}", days.join(",")),
        "absoluteYearly" => format!("FREQ=YEARLY;BYMONTH={month};BYMONTHDAY={mday}"),
        "relativeYearly" => format!("FREQ=YEARLY;BYMONTH={month};BYDAY={};BYSETPOS={setpos}", days.join(",")),
        _ => return None,
    };
    if interval > 1 {
        rule.push_str(&format!(";INTERVAL={interval}"));
    }
    let range = r.get("range");
    match range.and_then(|g| g.get("type")).and_then(Value::as_str) {
        Some("endDate") => {
            if let Some(d) = range.and_then(|g| g.get("endDate")).and_then(Value::as_str).and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            {
                rule.push_str(&format!(";UNTIL={}T235959Z", d.format("%Y%m%d")));
            }
        }
        Some("numbered") => {
            if let Some(n) = range.and_then(|g| g.get("numberOfOccurrences")).and_then(Value::as_i64) {
                rule.push_str(&format!(";COUNT={n}"));
            }
        }
        _ => {}
    }
    Some(rule)
}

/// An RRULE as Graph's repeat pattern, for the rules MailCraft's editor makes (and the common
/// ones from elsewhere). None for a rule Graph cannot say.
pub fn rrule_to_graph(rule: &str, start: NaiveDate) -> Option<Value> {
    let parts: HashMap<String, String> =
        rule.split(';').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.trim().to_ascii_uppercase(), v.trim().to_ascii_uppercase())).collect();
    let interval: i64 = parts.get("INTERVAL").and_then(|i| i.parse().ok()).unwrap_or(1);
    let byday: Vec<&'static str> = parts
        .get("BYDAY")
        .map(|d| {
            d.split(',')
                .filter_map(|x| {
                    let code = x.trim_start_matches(|c: char| c == '-' || c == '+' || c.is_ascii_digit());
                    DAYS.iter().find(|y| y.0 == code).map(|y| y.1)
                })
                .collect()
        })
        .unwrap_or_default();
    let start_day = DAYS.iter().find(|d| d.2 == start.weekday()).map(|d| d.1).unwrap_or("monday");
    let pattern = match parts.get("FREQ").map(String::as_str)? {
        "DAILY" => json!({ "type": "daily", "interval": interval }),
        "WEEKLY" => {
            let days = if byday.is_empty() { vec![start_day] } else { byday };
            json!({ "type": "weekly", "interval": interval, "daysOfWeek": days, "firstDayOfWeek": "monday" })
        }
        "MONTHLY" if parts.contains_key("BYDAY") || parts.contains_key("BYSETPOS") => return None,
        "MONTHLY" => {
            let d: i64 = parts.get("BYMONTHDAY").and_then(|x| x.parse().ok()).unwrap_or(i64::from(start.day()));
            json!({ "type": "absoluteMonthly", "interval": interval, "dayOfMonth": d })
        }
        "YEARLY" if parts.contains_key("BYDAY") => return None,
        "YEARLY" => {
            let m: i64 = parts.get("BYMONTH").and_then(|x| x.parse().ok()).unwrap_or(i64::from(start.month()));
            let d: i64 = parts.get("BYMONTHDAY").and_then(|x| x.parse().ok()).unwrap_or(i64::from(start.day()));
            json!({ "type": "absoluteYearly", "interval": interval, "month": m, "dayOfMonth": d })
        }
        _ => return None,
    };
    let start_s = start.format("%Y-%m-%d").to_string();
    let range = if let Some(n) = parts.get("COUNT").and_then(|c| c.parse::<i64>().ok()) {
        json!({ "type": "numbered", "startDate": start_s, "numberOfOccurrences": n })
    } else if let Some(u) = parts.get("UNTIL").and_then(|u| NaiveDate::parse_from_str(u.get(..8).unwrap_or(""), "%Y%m%d").ok()) {
        json!({ "type": "endDate", "startDate": start_s, "endDate": u.format("%Y-%m-%d").to_string() })
    } else {
        json!({ "type": "noEnd", "startDate": start_s })
    };
    Some(json!({ "pattern": pattern, "range": range }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_graph_event() {
        let v = json!({
            "id": "AAMk=", "iCalUId": "uid-1", "subject": "Team catch-up", "body": { "contentType": "text", "content": " Agenda \n" },
            "start": { "dateTime": "2026-10-12T09:00:00.0000000", "timeZone": "UTC" },
            "end": { "dateTime": "2026-10-12T09:30:00.0000000", "timeZone": "UTC" },
            "isAllDay": false, "location": { "displayName": "Room 2" }, "isReminderOn": true, "reminderMinutesBeforeStart": 10,
            "showAs": "busy", "type": "singleInstance", "changeKey": "ck1",
            "organizer": { "emailAddress": { "name": "Mark", "address": "mark@example.com" } },
            "attendees": [{ "emailAddress": { "name": "Jo", "address": "jo@example.com" }, "status": { "response": "tentativelyAccepted" } }]
        });
        let e = from_graph(&v);
        assert_eq!(e.uid, "uid-1");
        assert_eq!(e.summary, "Team catch-up");
        assert_eq!(e.description, "Agenda");
        assert_eq!(e.location, "Room 2");
        assert_eq!(e.alarms, vec![10]);
        assert_eq!(
            e.start,
            When::Time(DateTime::from_naive_utc_and_offset(NaiveDate::from_ymd_opt(2026, 10, 12).unwrap().and_hms_opt(9, 0, 0).unwrap(), Utc))
        );
        assert_eq!(e.attendees[0].partstat, "TENTATIVE");
        assert_eq!(e.organizer.map(|o| o.email), Some("mark@example.com".into()));
        // Round trip through iCalendar, as the cache keeps it.
        let back = ical::parse(&ical::to_ics(&from_graph(&v), &[]));
        assert_eq!(back.first().map(|e| e.summary.clone()), Some("Team catch-up".into()));
    }

    #[test]
    fn all_day_and_occurrences() {
        let v = json!({
            "id": "x", "iCalUId": "series-1", "subject": "Holiday", "isAllDay": true, "type": "occurrence",
            "start": { "dateTime": "2026-12-24T00:00:00.0000000" }, "end": { "dateTime": "2026-12-26T00:00:00.0000000" }
        });
        let e = from_graph(&v);
        assert_eq!(e.start, When::Date(NaiveDate::from_ymd_opt(2026, 12, 24).unwrap()));
        assert_eq!(e.end, When::Date(NaiveDate::from_ymd_opt(2026, 12, 26).unwrap()));
        // Occurrences of one series must not share a uid.
        assert!(e.uid.starts_with("series-1-") && e.uid.len() > "series-1-".len());
    }

    #[test]
    fn repeat_patterns_both_ways() {
        let weekly = json!({ "pattern": { "type": "weekly", "interval": 2, "daysOfWeek": ["monday", "thursday"] }, "range": { "type": "numbered", "numberOfOccurrences": 6 } });
        assert_eq!(rrule_from_graph(&weekly).as_deref(), Some("FREQ=WEEKLY;BYDAY=MO,TH;INTERVAL=2;COUNT=6"));
        let last_fri = json!({ "pattern": { "type": "relativeMonthly", "interval": 1, "daysOfWeek": ["friday"], "index": "last" }, "range": { "type": "noEnd" } });
        assert_eq!(rrule_from_graph(&last_fri).as_deref(), Some("FREQ=MONTHLY;BYDAY=FR;BYSETPOS=-1"));

        let start = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap(); // a Monday
        let g = rrule_to_graph("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR", start).unwrap();
        assert_eq!(g["pattern"]["daysOfWeek"], json!(["monday", "tuesday", "wednesday", "thursday", "friday"]));
        assert_eq!(g["range"]["type"], "noEnd");
        let g = rrule_to_graph("FREQ=YEARLY;UNTIL=20301012T000000Z", start).unwrap();
        assert_eq!(g["pattern"], json!({ "type": "absoluteYearly", "interval": 1, "month": 10, "dayOfMonth": 12 }));
        assert_eq!(g["range"]["endDate"], "2030-10-12");
        assert_eq!(rrule_to_graph("FREQ=MONTHLY;BYDAY=2TU", start), None);
    }

    #[test]
    fn writes_a_graph_event() {
        let e = Event {
            summary: "Dentist".into(),
            start: When::Date(NaiveDate::from_ymd_opt(2026, 11, 3).unwrap()),
            end: When::Date(NaiveDate::from_ymd_opt(2026, 11, 3).unwrap()),
            alarms: vec![30],
            rrule: Some("FREQ=DAILY;COUNT=2".into()),
            ..Event::default()
        };
        let v = to_graph(&e, true);
        assert_eq!(v["isAllDay"], true);
        assert_eq!(v["start"]["dateTime"], "2026-11-03T00:00:00");
        // An all-day end on the same day becomes the next midnight.
        assert_eq!(v["end"]["dateTime"], "2026-11-04T00:00:00");
        assert_eq!(v["reminderMinutesBeforeStart"], 30);
        assert_eq!(v["recurrence"]["range"]["numberOfOccurrences"], 2);
        // Changing an existing event leaves its repeat alone.
        assert!(to_graph(&e, false).get("recurrence").is_none());
    }
}
