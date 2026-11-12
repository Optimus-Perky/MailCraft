//! iCalendar (RFC 5545) events: read from and written to the text CalDAV servers and invitations
//! carry, and repeating events laid out over a stretch of days (with the `rrule` crate).
//!
//! Only what a calendar needs is read: the VEVENTs (summary, place, notes, start and end with
//! their time zone, all-day, repeat rule and exceptions, reminders, organiser and attendees).
//! Writing keeps the event's own text for everything not edited, so properties MailCraft does
//! not know about survive a change.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
pub use rrule::Tz;

/// A start or end: a whole day, a moment, or a wall-clock time with no zone ("floating").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Date(NaiveDate),
    Time(DateTime<Utc>),
    Floating(NaiveDateTime),
}

impl When {
    /// As a moment (a whole day starts at midnight in `local`; a floating time is taken there).
    pub fn utc(self, local: Tz) -> DateTime<Utc> {
        match self {
            When::Time(t) => t,
            When::Date(d) => at_local(local, d.and_time(NaiveTime::MIN)),
            When::Floating(n) => at_local(local, n),
        }
    }

    pub fn is_date(self) -> bool {
        matches!(self, When::Date(_))
    }
}

fn at_local(tz: Tz, n: NaiveDateTime) -> DateTime<Utc> {
    tz.from_local_datetime(&n).earliest().map(|t| t.with_timezone(&Utc)).unwrap_or_else(|| Utc.from_utc_datetime(&n))
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Attendee {
    pub email: String,
    pub name: String,
    /// NEEDS-ACTION, ACCEPTED, DECLINED, TENTATIVE...
    pub partstat: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub uid: String,
    pub summary: String,
    pub location: String,
    pub description: String,
    pub start: When,
    pub end: When,
    /// The zone the start was written in (kept when the event is written back).
    pub tzid: Option<String>,
    /// RRULE value ("FREQ=WEEKLY;BYDAY=MO").
    pub rrule: Option<String>,
    pub exdates: Vec<When>,
    /// Set on a changed instance of a repeating event: which one it replaces.
    pub recurrence_id: Option<When>,
    /// Reminders: minutes before the start.
    pub alarms: Vec<i64>,
    pub organizer: Option<Attendee>,
    pub attendees: Vec<Attendee>,
    /// CONFIRMED, TENTATIVE or CANCELLED.
    pub status: String,
    pub sequence: i64,
    /// TRANSPARENT: does not make the time busy.
    pub transparent: bool,
}

impl Default for Event {
    fn default() -> Self {
        let now = Utc::now();
        Event {
            uid: String::new(),
            summary: String::new(),
            location: String::new(),
            description: String::new(),
            start: When::Time(now),
            end: When::Time(now + Duration::hours(1)),
            tzid: None,
            rrule: None,
            exdates: vec![],
            recurrence_id: None,
            alarms: vec![],
            organizer: None,
            attendees: vec![],
            status: String::new(),
            sequence: 0,
            transparent: false,
        }
    }
}

/// A property line: name, parameters (upper-cased names), value (still escaped).
struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn param(&self, n: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str())
    }
}

/// Undo line folding (a line starting with a space or tab continues the one before).
fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix([' ', '\t'])
            && let Some(last) = out.last_mut()
        {
            last.push_str(rest);
        } else if !line.is_empty() {
            out.push(line.to_string());
        }
    }
    out
}

fn parse_line(line: &str) -> Option<Prop> {
    // The value starts at the first colon outside quotes.
    let mut in_quotes = false;
    let mut split = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let at = split?;
    let (head, value) = (line.get(..at)?, line.get(at + 1..)?);
    let mut parts = vec![];
    let mut cur = String::new();
    let mut q = false;
    for c in head.chars() {
        match c {
            '"' => q = !q,
            ';' if !q => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    let mut it = parts.into_iter();
    let name = it.next()?.to_ascii_uppercase();
    let params = it.filter_map(|p| p.split_once('=').map(|(k, v)| (k.to_ascii_uppercase(), v.to_string()))).collect();
    Some(Prop { name, params, value: value.to_string() })
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace("\r\n", "\\n").replace('\n', "\\n")
}

/// Windows zone names (Outlook's invitations) to IANA, for the common ones.
fn windows_zone(name: &str) -> Option<&'static str> {
    Some(match name {
        "GMT Standard Time" | "Greenwich Standard Time" => "Europe/London",
        "W. Europe Standard Time" => "Europe/Berlin",
        "Romance Standard Time" => "Europe/Paris",
        "Central Europe Standard Time" => "Europe/Budapest",
        "Central European Standard Time" => "Europe/Warsaw",
        "E. Europe Standard Time" => "Europe/Chisinau",
        "FLE Standard Time" => "Europe/Kiev",
        "GTB Standard Time" => "Europe/Bucharest",
        "Eastern Standard Time" => "America/New_York",
        "Central Standard Time" => "America/Chicago",
        "Mountain Standard Time" => "America/Denver",
        "Pacific Standard Time" => "America/Los_Angeles",
        "India Standard Time" => "Asia/Kolkata",
        "China Standard Time" => "Asia/Shanghai",
        "Tokyo Standard Time" => "Asia/Tokyo",
        "AUS Eastern Standard Time" => "Australia/Sydney",
        "UTC" | "Coordinated Universal Time" => "UTC",
        _ => return None,
    })
}

/// A zone by name: IANA ("Europe/London"), Windows ("GMT Standard Time"), or a prefixed form
/// some servers write ("/mozilla.org/.../Europe/London").
pub fn zone(name: &str) -> Option<Tz> {
    let n = name.trim().trim_matches('"');
    let n = windows_zone(n).unwrap_or(n);
    if let Ok(z) = n.parse::<chrono_tz::Tz>() {
        return Some(Tz::Tz(z));
    }
    // The last two parts of a path ("Europe/London").
    let parts: Vec<&str> = n.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2
        && let Some(tail) = parts.get(parts.len() - 2..)
        && let Ok(z) = tail.join("/").parse::<chrono_tz::Tz>()
    {
        return Some(Tz::Tz(z));
    }
    None
}

/// The PC's zone name ("Europe/London"), when the system says and it is a known one.
pub fn local_zone_name() -> Option<String> {
    iana_time_zone::get_timezone().ok().filter(|n| zone(n).is_some())
}

/// The PC's zone (IANA name from the system), else UTC.
pub fn local_zone() -> Tz {
    iana_time_zone::get_timezone().ok().and_then(|n| zone(&n)).unwrap_or(Tz::UTC)
}

fn parse_when(p: &Prop) -> Option<When> {
    let v = p.value.trim();
    let date_only = p.param("VALUE").is_some_and(|x| x.eq_ignore_ascii_case("DATE")) || (v.len() == 8 && !v.contains('T'));
    if date_only {
        return NaiveDate::parse_from_str(v.get(..8)?, "%Y%m%d").ok().map(When::Date);
    }
    let utc = v.ends_with('Z');
    let n = NaiveDateTime::parse_from_str(v.trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok()?;
    if utc {
        return Some(When::Time(Utc.from_utc_datetime(&n)));
    }
    match p.param("TZID").and_then(zone) {
        Some(tz) => Some(When::Time(at_local(tz, n))),
        None => Some(When::Floating(n)),
    }
}

/// "PT15M", "-PT1H", "P1D", "P1W", "PT1H30M": seconds (negative before).
fn parse_duration(v: &str) -> Option<i64> {
    let v = v.trim();
    let (neg, rest) = match v.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let rest = rest.strip_prefix('P')?;
    let mut secs = 0i64;
    let mut num = String::new();
    let mut in_time = false;
    for c in rest.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                secs += n * match (c, in_time) {
                    ('W', _) => 7 * 86400,
                    ('D', _) => 86400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
        }
    }
    Some(if neg { -secs } else { secs })
}

fn attendee(p: &Prop) -> Attendee {
    Attendee {
        email: p.value.trim().trim_start_matches("mailto:").trim_start_matches("MAILTO:").to_string(),
        name: p.param("CN").unwrap_or("").trim_matches('"').to_string(),
        partstat: p.param("PARTSTAT").unwrap_or("NEEDS-ACTION").to_ascii_uppercase(),
    }
}

/// Every VEVENT in a calendar's text (a repeating event's changed instances come as their own,
/// with recurrence_id set).
pub fn parse(text: &str) -> Vec<Event> {
    let mut out = vec![];
    let mut cur: Option<Event> = None;
    let mut duration: Option<i64> = None;
    let mut has_end = false;
    // Inside a VALARM: its trigger.
    let mut in_alarm = false;
    let mut depth_other = 0usize;
    for line in unfold(text) {
        let Some(p) = parse_line(&line) else { continue };
        match (p.name.as_str(), p.value.trim().to_ascii_uppercase().as_str()) {
            ("BEGIN", "VEVENT") if cur.is_none() => {
                cur = Some(Event { start: When::Floating(NaiveDateTime::MIN), end: When::Floating(NaiveDateTime::MIN), ..Default::default() });
                duration = None;
                has_end = false;
                continue;
            }
            ("BEGIN", "VALARM") if cur.is_some() => {
                in_alarm = true;
                continue;
            }
            ("END", "VALARM") => {
                in_alarm = false;
                continue;
            }
            ("BEGIN", _) if cur.is_some() => {
                depth_other += 1;
                continue;
            }
            ("END", "VEVENT") => {
                if let Some(mut e) = cur.take() {
                    if !has_end {
                        e.end = match (e.start, duration) {
                            (When::Date(d), d2) => When::Date(d + Duration::seconds(d2.unwrap_or(86400).max(86400))),
                            (When::Time(t), d) => When::Time(t + Duration::seconds(d.unwrap_or(0))),
                            (When::Floating(n), d) => When::Floating(n + Duration::seconds(d.unwrap_or(0))),
                        };
                    }
                    if !e.uid.is_empty() && e.start != When::Floating(NaiveDateTime::MIN) {
                        out.push(e);
                    }
                }
                continue;
            }
            ("END", _) if depth_other > 0 => {
                depth_other -= 1;
                continue;
            }
            _ => {}
        }
        let Some(e) = cur.as_mut() else { continue };
        if depth_other > 0 {
            continue;
        }
        if in_alarm {
            if p.name == "TRIGGER"
                && !p.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME"))
                && let Some(s) = parse_duration(&p.value)
                && !p.param("RELATED").is_some_and(|r| r.eq_ignore_ascii_case("END"))
            {
                e.alarms.push((-s / 60).max(0));
            }
            continue;
        }
        match p.name.as_str() {
            "UID" => e.uid = p.value.trim().to_string(),
            "SUMMARY" => e.summary = unescape(&p.value),
            "LOCATION" => e.location = unescape(&p.value),
            "DESCRIPTION" => e.description = unescape(&p.value),
            "DTSTART" => {
                if let Some(w) = parse_when(&p) {
                    e.start = w;
                    e.tzid = p.param("TZID").and_then(|z| zone(z).map(|_| z.trim_matches('"').to_string()));
                }
            }
            "DTEND" => {
                if let Some(w) = parse_when(&p) {
                    e.end = w;
                    has_end = true;
                }
            }
            "DURATION" => duration = parse_duration(&p.value),
            "RRULE" => e.rrule = Some(p.value.trim().to_string()),
            "EXDATE" => {
                for v in p.value.split(',') {
                    let one = Prop { name: p.name.clone(), params: p.params.clone(), value: v.to_string() };
                    if let Some(w) = parse_when(&one) {
                        e.exdates.push(w);
                    }
                }
            }
            "RECURRENCE-ID" => e.recurrence_id = parse_when(&p),
            "ORGANIZER" => e.organizer = Some(attendee(&p)),
            "ATTENDEE" => e.attendees.push(attendee(&p)),
            "STATUS" => e.status = p.value.trim().to_ascii_uppercase(),
            "SEQUENCE" => e.sequence = p.value.trim().parse().unwrap_or(0),
            "TRANSP" => e.transparent = p.value.trim().eq_ignore_ascii_case("TRANSPARENT"),
            _ => {}
        }
    }
    out
}

/// The METHOD of an invitation (REQUEST, REPLY, CANCEL...), if it has one.
pub fn method(text: &str) -> Option<String> {
    unfold(text).iter().filter_map(|l| parse_line(l)).find(|p| p.name == "METHOD").map(|p| p.value.trim().to_ascii_uppercase())
}

/// Lines longer than 75 octets folded, as the standard asks.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut n = 0;
    for c in line.chars() {
        let w = c.len_utf8();
        if n + w > 75 {
            out.push_str("\r\n ");
            n = 1;
        }
        out.push(c);
        n += w;
    }
    out.push_str("\r\n");
    out
}

fn when_prop(name: &str, w: When, tzid: Option<&str>) -> String {
    match w {
        When::Date(d) => format!("{name};VALUE=DATE:{}", d.format("%Y%m%d")),
        When::Floating(n) => format!("{name}:{}", n.format("%Y%m%dT%H%M%S")),
        When::Time(t) => match tzid.and_then(|z| zone(z).map(|tz| (z, tz))) {
            // In its own zone, so a repeat keeps its wall-clock time over the clock changes.
            Some((z, tz)) => format!("{name};TZID={z}:{}", t.with_timezone(&tz).format("%Y%m%dT%H%M%S")),
            None => format!("{name}:{}", t.format("%Y%m%dT%H%M%SZ")),
        },
    }
}

/// An event as a whole calendar object (VCALENDAR with one VEVENT, plus `extra` VEVENTs: a
/// repeating event's changed instances, kept as they were).
pub fn to_ics(e: &Event, extra: &[Event]) -> String {
    let mut out = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//MailCraft//Calendar//EN\r\nCALSCALE:GREGORIAN\r\n");
    for ev in std::iter::once(e).chain(extra.iter()) {
        out.push_str(&vevent(ev));
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

/// A scheduling message (iTIP, RFC 5546): METHOD REQUEST, REPLY or CANCEL with the event.
pub fn to_itip(method: &str, e: &Event) -> String {
    let mut out = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//MailCraft//Calendar//EN\r\nCALSCALE:GREGORIAN\r\nMETHOD:{}\r\n",
        method.trim().to_ascii_uppercase()
    );
    out.push_str(&vevent(e));
    out.push_str("END:VCALENDAR\r\n");
    out
}

/// The reply to an invitation from `me` (an address), with my answer (ACCEPTED, TENTATIVE or
/// DECLINED): the meeting as the organiser sent it, with me as its only attendee.
pub fn reply(invite: &Event, me: &str, name: &str, partstat: &str) -> Event {
    let mine = invite.attendees.iter().find(|a| a.email.eq_ignore_ascii_case(me));
    let attendee = Attendee {
        email: mine.map(|a| a.email.clone()).unwrap_or_else(|| me.to_string()),
        name: mine.map(|a| a.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| name.to_string()),
        partstat: partstat.to_string(),
    };
    Event { attendees: vec![attendee], alarms: vec![], description: String::new(), ..invite.clone() }
}

/// My answer recorded on my copy of a meeting: my attendee line set to `partstat` (added when
/// the invitation did not name me, e.g. it came through a list).
pub fn with_my_answer(invite: &Event, me: &str, name: &str, partstat: &str) -> Event {
    let mut e = invite.clone();
    match e.attendees.iter_mut().find(|a| a.email.eq_ignore_ascii_case(me)) {
        Some(a) => a.partstat = partstat.to_string(),
        None => e.attendees.push(Attendee { email: me.to_string(), name: name.to_string(), partstat: partstat.to_string() }),
    }
    if partstat == "TENTATIVE" {
        e.status = "TENTATIVE".into();
    }
    e
}

fn vevent(e: &Event) -> String {
    let mut lines: Vec<String> = vec!["BEGIN:VEVENT".into(), format!("UID:{}", e.uid)];
    lines.push(format!("DTSTAMP:{}", Utc::now().format("%Y%m%dT%H%M%SZ")));
    if let Some(r) = e.recurrence_id {
        lines.push(when_prop("RECURRENCE-ID", r, e.tzid.as_deref()));
    }
    lines.push(when_prop("DTSTART", e.start, e.tzid.as_deref()));
    lines.push(when_prop("DTEND", e.end, e.tzid.as_deref()));
    if !e.summary.is_empty() {
        lines.push(format!("SUMMARY:{}", escape(&e.summary)));
    }
    if !e.location.is_empty() {
        lines.push(format!("LOCATION:{}", escape(&e.location)));
    }
    if !e.description.is_empty() {
        lines.push(format!("DESCRIPTION:{}", escape(&e.description)));
    }
    if let Some(r) = &e.rrule {
        lines.push(format!("RRULE:{r}"));
    }
    for x in &e.exdates {
        lines.push(when_prop("EXDATE", *x, e.tzid.as_deref()));
    }
    if !e.status.is_empty() {
        lines.push(format!("STATUS:{}", e.status));
    }
    if e.sequence > 0 {
        lines.push(format!("SEQUENCE:{}", e.sequence));
    }
    if e.transparent {
        lines.push("TRANSP:TRANSPARENT".into());
    }
    let person = |name: &str, a: &Attendee, with_status: bool| {
        let mut s = name.to_string();
        if !a.name.is_empty() {
            s.push_str(&format!(";CN=\"{}\"", a.name.replace('"', "'")));
        }
        if with_status {
            s.push_str(&format!(";PARTSTAT={};RSVP=TRUE", if a.partstat.is_empty() { "NEEDS-ACTION" } else { &a.partstat }));
        }
        s.push_str(&format!(":mailto:{}", a.email));
        s
    };
    if let Some(o) = &e.organizer {
        lines.push(person("ORGANIZER", o, false));
    }
    for a in &e.attendees {
        lines.push(person("ATTENDEE", a, true));
    }
    for m in &e.alarms {
        lines.extend([
            "BEGIN:VALARM".into(),
            "ACTION:DISPLAY".into(),
            format!("DESCRIPTION:{}", escape(&e.summary)),
            format!("TRIGGER:-PT{m}M"),
            "END:VALARM".into(),
        ]);
    }
    lines.push("END:VEVENT".into());
    lines.iter().map(|l| fold(l)).collect()
}

/// One showing of an event: when it starts and ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurrence {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// The instance it is, for a repeating event (its original start).
    pub instance: Option<DateTime<Utc>>,
}

/// When an event happens between `from` and `to`: once, or (repeating) each time, less the
/// exceptions and the instances `overridden` (changed instances, shown as their own events).
pub fn occurrences(e: &Event, from: DateTime<Utc>, to: DateTime<Utc>, local: Tz, overridden: &[DateTime<Utc>]) -> Vec<Occurrence> {
    let start = e.start.utc(local);
    let length = e.end.utc(local) - start;
    let length = if length < Duration::zero() { Duration::zero() } else { length };
    let Some(rule) = e.rrule.as_deref().filter(|_| e.recurrence_id.is_none()) else {
        let end = start + length;
        let shows = if e.start.is_date() { start < to && end > from } else { start < to && (end > from || (length.is_zero() && start >= from)) };
        return if shows { vec![Occurrence { start, end, instance: None }] } else { vec![] };
    };
    // The repeat is laid out in the event's own zone (all-day and floating: the local one).
    let tz = match e.start {
        When::Time(_) => e.tzid.as_deref().and_then(zone).unwrap_or(Tz::UTC),
        _ => local,
    };
    let dtstart = start.with_timezone(&tz);
    let Ok(rrule) = rule.parse::<rrule::RRule<rrule::Unvalidated>>() else {
        return vec![Occurrence { start, end: start + length, instance: None }];
    };
    let Ok(set) = rrule.build(dtstart) else {
        return vec![Occurrence { start, end: start + length, instance: None }];
    };
    let ex: Vec<DateTime<Utc>> = e.exdates.iter().map(|x| x.utc(local)).collect();
    // Starts from `from - length`, so one that began before the range but runs into it shows.
    let after = (from - length - Duration::seconds(1)).with_timezone(&tz);
    let before = to.with_timezone(&tz);
    let result = set.after(after).before(before).all(5000);
    result
        .dates
        .into_iter()
        .map(|d| d.with_timezone(&Utc))
        .filter(|d| !ex.iter().any(|x| x == d || (e.start.is_date() && x.date_naive() == d.date_naive())))
        .filter(|d| !overridden.contains(d))
        .filter(|d| *d < to && *d + length > from)
        .map(|d| Occurrence { start: d, end: d + length, instance: Some(d) })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn answers_an_invitation() {
        let invite = Event {
            uid: "m-1@example.org".into(),
            summary: "Planning".into(),
            sequence: 2,
            organizer: Some(Attendee { email: "boss@example.org".into(), name: "Boss".into(), partstat: String::new() }),
            attendees: vec![
                Attendee { email: "Mark@Example.com".into(), name: "Mark".into(), partstat: "NEEDS-ACTION".into() },
                Attendee { email: "jo@example.org".into(), name: "Jo".into(), partstat: "ACCEPTED".into() },
            ],
            ..Event::default()
        };
        let r = reply(&invite, "mark@example.com", "Sam Taylor", "ACCEPTED");
        assert_eq!(r.attendees.len(), 1);
        assert_eq!((r.attendees[0].email.as_str(), r.attendees[0].partstat.as_str()), ("Mark@Example.com", "ACCEPTED"));
        let text = to_itip("REPLY", &r);
        assert!(text.contains("METHOD:REPLY\r\n"));
        assert!(text.contains("UID:m-1@example.org"));
        assert!(text.contains("SEQUENCE:2"));
        assert!(text.contains("PARTSTAT=ACCEPTED"));
        assert!(text.contains("ORGANIZER;CN=\"Boss\":mailto:boss@example.org"));
        assert_eq!(method(&text).as_deref(), Some("REPLY"));
        // My copy keeps everyone, with my answer.
        let mine = with_my_answer(&invite, "mark@example.com", "Mark", "TENTATIVE");
        assert_eq!(mine.attendees.len(), 2);
        assert_eq!(mine.attendees[0].partstat, "TENTATIVE");
        assert_eq!(with_my_answer(&invite, "other@example.com", "Me", "ACCEPTED").attendees.len(), 3);
    }

    use super::*;

    fn london() -> Tz {
        zone("Europe/London").unwrap()
    }

    const SAMPLE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/London\r\nBEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0000\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:abc@x\r\nSUMMARY:Bins out\\, both of them\r\nDTSTART;TZID=Europe/London:20261012T190000\r\nDURATION:PT30M\r\nRRULE:FREQ=WEEKLY;BYDAY=MO\r\nEXDATE;TZID=Europe/London:20261019T190000\r\n\
DESCRIPTION:Blue bin\\nand green\r\nBEGIN:VALARM\r\nTRIGGER:-PT15M\r\nACTION:DISPLAY\r\nEND:VALARM\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:day@x\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20261024\r\nDTEND;VALUE=DATE:20261026\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:inv@x\r\nSUMMARY:Review\r\nDTSTART;TZID=GMT Standard Time:20261014T100000\r\nDTEND;TZID=GMT Standard Time:20261014T110000\r\n\
ORGANIZER;CN=Ann Lee:mailto:ann@example.org\r\nATTENDEE;CN=Mark;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:mark@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn reads_events() {
        let ev = parse(SAMPLE);
        assert_eq!(ev.len(), 3);
        let bins = &ev[0];
        assert_eq!(bins.summary, "Bins out, both of them");
        assert_eq!(bins.description, "Blue bin\nand green");
        assert_eq!(bins.alarms, vec![15]);
        // 19:00 BST is 18:00 UTC; 30 minutes long.
        assert_eq!(bins.start, When::Time(Utc.with_ymd_and_hms(2026, 10, 12, 18, 0, 0).unwrap()));
        assert_eq!(bins.end, When::Time(Utc.with_ymd_and_hms(2026, 10, 12, 18, 30, 0).unwrap()));
        assert_eq!(ev[1].start, When::Date(NaiveDate::from_ymd_opt(2026, 10, 24).unwrap()));
        assert!(ev[1].end.is_date());
        // Outlook's Windows zone name.
        assert_eq!(ev[2].start, When::Time(Utc.with_ymd_and_hms(2026, 10, 14, 9, 0, 0).unwrap()));
        assert_eq!(ev[2].organizer.as_ref().map(|o| o.name.as_str()), Some("Ann Lee"));
        assert_eq!(ev[2].attendees.first().map(|a| a.partstat.as_str()), Some("NEEDS-ACTION"));
    }

    #[test]
    fn repeats_keep_their_wall_clock_over_the_clock_change() {
        let ev = parse(SAMPLE);
        let from = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 11, 10, 0, 0, 0).unwrap();
        let occ = occurrences(&ev[0], from, to, london(), &[]);
        let starts: Vec<String> = occ.iter().map(|o| o.start.with_timezone(&london()).format("%d %H:%M").to_string()).collect();
        // Mondays at 19:00 local; the 19th is excepted; the clocks go back on 25 October.
        assert_eq!(starts, vec!["12 19:00", "26 19:00", "02 19:00", "09 19:00"]);
        // A changed instance is left out of the series.
        let skip = occ[1].start;
        assert_eq!(occurrences(&ev[0], from, to, london(), &[skip]).len(), 3);
    }

    #[test]
    fn writes_and_reads_back() {
        let mut e = parse(SAMPLE).remove(0);
        e.summary = "Bins; all three".into();
        e.location = "Front, by the gate".into();
        let text = to_ics(&e, &[]);
        assert!(text.contains("DTSTART;TZID=Europe/London:20261012T190000"), "{text}");
        assert!(text.lines().all(|l| l.len() <= 76), "folded");
        let back = parse(&text).remove(0);
        assert_eq!((back.summary.as_str(), back.location.as_str()), ("Bins; all three", "Front, by the gate"));
        assert_eq!((back.start, back.end, back.rrule.clone(), back.alarms.clone()), (e.start, e.end, e.rrule.clone(), e.alarms.clone()));
        assert_eq!(back.exdates, e.exdates);
        let long = Event { uid: "u".into(), summary: "x".repeat(200), ..Default::default() };
        assert_eq!(parse(&to_ics(&long, &[]))[0].summary.len(), 200);
    }

    #[test]
    fn all_day_and_plain() {
        let ev = parse(SAMPLE);
        let from = Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 10, 26, 0, 0, 0).unwrap();
        assert_eq!(occurrences(&ev[1], from, to, london(), &[]).len(), 1, "the holiday's second day");
        assert!(occurrences(&ev[2], from, to, london(), &[]).is_empty());
        assert_eq!(parse_duration("-PT1H30M"), Some(-5400));
        assert_eq!(parse_duration("P1W"), Some(604800));
        assert!(method("BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR").is_some_and(|m| m == "REQUEST"));
    }
}
