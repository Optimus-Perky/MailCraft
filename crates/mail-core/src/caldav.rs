//! CalDAV (RFC 4791): finding a person's calendars on a server, reading their events, and
//! writing changes back. Works with Nextcloud, Radicale, Poste.io (when DAV is on), Google
//! (with a sign-in token) and any other CalDAV server.
//!
//! Finding: the server's /.well-known/caldav (RFC 6764) or the address given, then
//! current-user-principal, then calendar-home-set, then the calendars in it. Syncing: each
//! calendar's ctag says whether anything changed; if it did, the etags say which events, and
//! only those are fetched (calendar-multiget). Writing: PUT with If-Match, so a change made
//! elsewhere in the meantime is not overwritten.

use std::time::Duration;

use base64::Engine;
use quick_xml::events::Event as Xml;
use ureq::http;

use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    Basic { user: String, pass: String },
    Bearer(String),
}

impl Auth {
    fn header(&self) -> String {
        match self {
            Auth::Basic { user, pass } => format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))),
            Auth::Bearer(t) => format!("Bearer {t}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Calendar {
    /// The calendar's full address (ends with "/").
    pub url: String,
    pub name: String,
    /// "#RRGGBB" (some servers add alpha: "#RRGGBBAA").
    pub colour: Option<String>,
    /// Changes whenever anything in the calendar does.
    pub ctag: Option<String>,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The event's full address.
    pub url: String,
    pub etag: String,
    /// Its iCalendar text (empty when only the etag was asked for).
    pub data: String,
}

/// One <response> of a multistatus: its href, and each property that came back 200.
#[derive(Debug, Default)]
struct Response {
    href: String,
    props: Vec<Prop>,
}

#[derive(Debug, Default, Clone)]
struct Prop {
    name: String,
    /// The text inside (all of it, with entities resolved).
    text: String,
    /// Every element name inside it, and every href.
    inner: Vec<String>,
    hrefs: Vec<String>,
    /// name="..." attributes inside (supported-calendar-component-set's comps).
    comps: Vec<String>,
}

impl Response {
    fn prop(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name == name)
    }
}

fn local(name: &[u8]) -> String {
    let s = String::from_utf8_lossy(name);
    s.rsplit(':').next().unwrap_or(&s).to_ascii_lowercase()
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => return None,
    })
}

/// A multistatus body as its responses (only the properties with a 2xx status).
fn parse_multistatus(body: &str) -> Result<Vec<Response>, Error> {
    let mut reader = quick_xml::Reader::from_str(body);
    let mut out = vec![];
    let mut path: Vec<String> = vec![];
    let mut cur: Option<Response> = None;
    // The properties of the propstat being read, and its status.
    let mut props: Vec<Prop> = vec![];
    let mut status_ok = true;
    let mut prop: Option<Prop> = None;
    let mut text = String::new();
    let bad = |e: quick_xml::Error| Error::Calendar(format!("the server's reply is not valid XML: {e}"));
    loop {
        let ev = reader.read_event().map_err(bad)?;
        match ev {
            Xml::Start(ref e) | Xml::Empty(ref e) => {
                let n = local(e.name().as_ref());
                let empty = matches!(ev, Xml::Empty(_));
                let parent = path.last().map(String::as_str);
                match (n.as_str(), parent) {
                    ("response", _) => cur = Some(Response::default()),
                    ("propstat", _) => {
                        props.clear();
                        status_ok = true;
                    }
                    (_, Some("prop")) if prop.is_none() => {
                        prop = Some(Prop { name: n.clone(), ..Default::default() });
                    }
                    _ => {
                        if let Some(p) = prop.as_mut() {
                            p.inner.push(n.clone());
                            if let Some(a) = e.try_get_attribute("name").ok().flatten() {
                                p.comps.push(String::from_utf8_lossy(&a.value).to_ascii_uppercase());
                            }
                        }
                    }
                }
                text.clear();
                if empty {
                    // An empty property element still counts (it was asked for and is blank).
                    if prop.as_ref().is_some_and(|p| p.name == n) && path.last().is_some_and(|l| l == "prop") {
                        props.extend(prop.take());
                    }
                } else {
                    path.push(n);
                }
            }
            Xml::Text(e) => text.push_str(&e.decode().map_err(|e| Error::Calendar(format!("reply text: {e}")))?),
            Xml::CData(e) => text.push_str(&String::from_utf8_lossy(&e)),
            Xml::GeneralRef(e) => {
                if e.is_char_ref() {
                    if let Ok(Some(c)) = e.resolve_char_ref() {
                        text.push(c);
                    }
                } else if let Some(c) = entity(&String::from_utf8_lossy(&e)) {
                    text.push(c);
                }
            }
            Xml::End(_) => {
                let n = path.pop().unwrap_or_default();
                let parent = path.last().map(String::as_str);
                if let Some(p) = prop.as_mut() {
                    if n == "href" {
                        p.hrefs.push(text.trim().to_string());
                    } else if p.name == n && parent == Some("prop") {
                        if p.text.is_empty() {
                            p.text = text.clone();
                        }
                        props.extend(prop.take());
                    }
                }
                match (n.as_str(), parent) {
                    ("href", Some("response")) => {
                        if let Some(r) = cur.as_mut() {
                            r.href = text.trim().to_string();
                        }
                    }
                    ("status", Some("propstat")) => status_ok = text.split_whitespace().nth(1).is_some_and(|c| c.starts_with('2')),
                    ("propstat", _) => {
                        if status_ok && let Some(r) = cur.as_mut() {
                            r.props.append(&mut props);
                        }
                        props.clear();
                    }
                    ("response", _) => out.extend(cur.take()),
                    _ => {}
                }
                text.clear();
            }
            Xml::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// `href` (absolute, or a path on the server) made absolute against `base`.
pub fn join(base: &str, href: &str) -> String {
    let href = href.trim();
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    // scheme://host[:port]
    let origin_end = base.find("://").map(|i| i + 3).and_then(|s| base.get(s..).map(|r| s + r.find('/').unwrap_or(r.len()))).unwrap_or(base.len());
    let origin = base.get(..origin_end).unwrap_or(base);
    if href.starts_with('/') {
        return format!("{origin}{href}");
    }
    let dir = base.rfind('/').filter(|&i| i >= origin_end).map(|i| base.get(..=i).unwrap_or(base)).unwrap_or(base);
    let sep = if dir.ends_with('/') { "" } else { "/" };
    format!("{dir}{sep}{href}")
}

/// Just the path of an address (to compare hrefs, which servers give either way).
fn path_of(url: &str) -> &str {
    match url.find("://") {
        Some(i) => url.get(i + 3..).and_then(|r| r.find('/').and_then(|j| r.get(j..))).unwrap_or("/"),
        None => url,
    }
}

pub struct Client {
    agent: ureq::Agent,
    auth: Auth,
    /// Plain http:// allowed (test servers only: the password would cross the network readable).
    pub insecure_ok: bool,
}

/// What came back: status, the address it came from (after redirects), the body and the ETag.
struct Reply {
    status: u16,
    url: String,
    body: String,
    etag: Option<String>,
    /// The DAV header: what the server can do.
    dav: Option<String>,
}

impl Client {
    pub fn new(auth: Auth) -> Client {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .allow_non_standard_methods(true)
            // Followed here, so the method and the sign-in go along.
            .max_redirects(0)
            .max_redirects_will_error(false)
            .build()
            .new_agent();
        Client { agent, auth, insecure_ok: false }
    }

    fn request(&self, method: &str, url: &str, depth: Option<&str>, headers: &[(&str, &str)], body: &str) -> Result<Reply, Error> {
        let mut url = url.to_string();
        for _ in 0..5 {
            if url.starts_with("http://") && !self.insecure_ok {
                return Err(Error::Insecure(url));
            }
            let m = http::Method::from_bytes(method.as_bytes()).map_err(|e| Error::Calendar(format!("{method}: {e}")))?;
            let mut b = http::Request::builder().method(m).uri(&url).header("Authorization", self.auth.header());
            if let Some(d) = depth {
                b = b.header("Depth", d);
            }
            if !body.is_empty() && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("Content-Type")) {
                b = b.header("Content-Type", "application/xml; charset=utf-8");
            }
            for (k, v) in headers {
                b = b.header(*k, *v);
            }
            let req = b.body(body.to_string()).map_err(|e| Error::Calendar(format!("{url}: {e}")))?;
            let mut resp = self.agent.run(req).map_err(|e| Error::Connect(url.clone(), e.to_string()))?;
            let status = resp.status().as_u16();
            if (300..400).contains(&status)
                && let Some(loc) = resp.headers().get("Location").and_then(|v| v.to_str().ok())
            {
                url = join(&url, loc);
                continue;
            }
            let etag = resp.headers().get("ETag").and_then(|v| v.to_str().ok()).map(str::to_string);
            let dav = resp.headers().get("DAV").and_then(|v| v.to_str().ok()).map(str::to_string);
            let body = resp.body_mut().with_config().limit(64 << 20).read_to_string().unwrap_or_default();
            return Ok(Reply { status, url, body, etag, dav });
        }
        Err(Error::Calendar(format!("{url}: too many redirects")))
    }

    fn propfind(&self, url: &str, depth: &str, props: &str) -> Result<(String, Vec<Response>), Error> {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<d:propfind xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\" xmlns:cs=\"http://calendarserver.org/ns/\" xmlns:ic=\"http://apple.com/ns/ical/\"><d:prop>{props}</d:prop></d:propfind>"
        );
        let r = self.request("PROPFIND", url, Some(depth), &[], &body)?;
        check(&r, url)?;
        Ok((r.url, parse_multistatus(&r.body)?))
    }

    /// The address of the person's calendar home, from a server address, a host name, or a
    /// calendar or principal address.
    pub fn discover(&self, start: &str) -> Result<String, Error> {
        let start = start.trim().trim_end_matches('/');
        let mut tries: Vec<String> = vec![];
        if start.starts_with("http://") || start.starts_with("https://") {
            tries.push(format!("{start}/"));
            if path_of(start).trim_matches('/').is_empty() {
                tries.push(format!("{start}/.well-known/caldav"));
            }
        } else {
            tries.push(format!("https://{start}/.well-known/caldav"));
            tries.push(format!("https://{start}/"));
        }
        let mut last: Option<Error> = None;
        for url in tries {
            match self.home_from(&url) {
                Ok(home) => return Ok(home),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| Error::Calendar("no calendar server found".into())))
    }

    fn home_from(&self, url: &str) -> Result<String, Error> {
        let (at, r) = self.propfind(url, "0", "<d:current-user-principal/><c:calendar-home-set/><d:resourcetype/>")?;
        let first = r.first().ok_or_else(|| Error::Calendar(format!("{url}: the server answered with nothing")))?;
        if let Some(home) = first.prop("calendar-home-set").and_then(|p| p.hrefs.first()) {
            return Ok(join(&at, home));
        }
        // A calendar itself: its parent is the home.
        if first.prop("resourcetype").is_some_and(|p| p.inner.iter().any(|n| n == "calendar")) {
            let me = at.trim_end_matches('/');
            return Ok(me.rfind('/').and_then(|i| me.get(..=i)).unwrap_or(&at).to_string());
        }
        let principal = first
            .prop("current-user-principal")
            .and_then(|p| p.hrefs.first())
            .ok_or_else(|| Error::Calendar(format!("{at}: not a calendar server (no principal)")))?;
        let purl = join(&at, principal);
        let (pat, r) = self.propfind(&purl, "0", "<c:calendar-home-set/>")?;
        r.first()
            .and_then(|x| x.prop("calendar-home-set"))
            .and_then(|p| p.hrefs.first())
            .map(|h| join(&pat, h))
            .ok_or_else(|| Error::Calendar(format!("{purl}: no calendar home")))
    }

    /// Whether the server sends meeting invitations and replies itself (RFC 6638
    /// calendar-auto-schedule, as Google and Nextcloud do): then MailCraft must not mail them too.
    pub fn auto_schedules(&self, url: &str) -> bool {
        self.request("OPTIONS", url, None, &[], "")
            .ok()
            .and_then(|r| r.dav)
            .is_some_and(|d| d.split(',').any(|x| x.trim().eq_ignore_ascii_case("calendar-auto-schedule")))
    }

    /// The calendars in a home (those that hold events).
    pub fn calendars(&self, home: &str) -> Result<Vec<Calendar>, Error> {
        let (at, r) = self.propfind(
            home,
            "1",
            "<d:resourcetype/><d:displayname/><ic:calendar-color/><cs:getctag/><c:supported-calendar-component-set/><d:current-user-privilege-set/>",
        )?;
        let mut out = vec![];
        for x in r {
            if !x.prop("resourcetype").is_some_and(|p| p.inner.iter().any(|n| n == "calendar")) {
                continue;
            }
            // Task lists and the like say so; a calendar saying nothing holds events.
            if let Some(c) = x.prop("supported-calendar-component-set")
                && !c.comps.is_empty()
                && !c.comps.iter().any(|n| n == "VEVENT")
            {
                continue;
            }
            let url = join(&at, &x.href);
            let name = x
                .prop("displayname")
                .map(|p| p.text.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| url.trim_end_matches('/').rsplit('/').next().unwrap_or("Calendar").to_string());
            let read_only =
                x.prop("current-user-privilege-set").is_some_and(|p| !p.inner.iter().any(|n| n == "write" || n == "write-content" || n == "all"));
            out.push(Calendar {
                url,
                name,
                colour: x.prop("calendar-color").map(|p| p.text.trim().to_string()).filter(|c| c.starts_with('#')),
                ctag: x.prop("getctag").map(|p| p.text.trim().to_string()).filter(|c| !c.is_empty()),
                read_only,
            });
        }
        Ok(out)
    }

    /// Every event's address and etag (no data).
    pub fn etags(&self, calendar: &str) -> Result<Vec<Item>, Error> {
        let (at, r) = self.propfind(calendar, "1", "<d:getetag/><d:resourcetype/>")?;
        let me = path_of(calendar).trim_end_matches('/').to_string();
        Ok(r.into_iter()
            .filter(|x| path_of(&join(&at, &x.href)).trim_end_matches('/') != me)
            .filter(|x| !x.prop("resourcetype").is_some_and(|p| p.inner.iter().any(|n| n == "collection")))
            .filter_map(|x| {
                let etag = x.prop("getetag").map(|p| p.text.trim().to_string())?;
                Some(Item { url: join(&at, &x.href), etag, data: String::new() })
            })
            .collect())
    }

    /// The given events in full (calendar-multiget), in batches.
    pub fn fetch(&self, calendar: &str, urls: &[String]) -> Result<Vec<Item>, Error> {
        let mut out = vec![];
        for chunk in urls.chunks(100) {
            let hrefs: String = chunk.iter().map(|u| format!("<d:href>{}</d:href>", xml_escape(path_of(u)))).collect();
            let body = format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<c:calendar-multiget xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><d:prop><d:getetag/><c:calendar-data/></d:prop>{hrefs}</c:calendar-multiget>"
            );
            let r = self.request("REPORT", calendar, Some("1"), &[], &body)?;
            check(&r, calendar)?;
            for x in parse_multistatus(&r.body)? {
                let (Some(etag), Some(data)) = (x.prop("getetag"), x.prop("calendar-data")) else { continue };
                out.push(Item { url: join(&r.url, &x.href), etag: etag.text.trim().to_string(), data: data.text.clone() });
            }
        }
        Ok(out)
    }

    /// Writes an event: a new one (`etag` None: only if nothing is there) or a change (only if it
    /// is still the version read). The new etag, when the server says.
    pub fn put(&self, url: &str, ics: &str, etag: Option<&str>) -> Result<Option<String>, Error> {
        let cond = match etag {
            Some(e) => ("If-Match", e),
            None => ("If-None-Match", "*"),
        };
        let r = self.request("PUT", url, None, &[("Content-Type", "text/calendar; charset=utf-8"), cond], ics)?;
        if r.status == 412 {
            return Err(Error::Calendar("the event was changed elsewhere; refresh and try again".into()));
        }
        check(&r, url)?;
        Ok(r.etag)
    }

    /// Makes a new calendar in a home (MKCALENDAR, or the extended MKCOL for servers that only
    /// take that), under a free address made from its name. Its address.
    pub fn make_calendar(&self, home: &str, name: &str, colour: Option<&str>) -> Result<String, Error> {
        let slug: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        let slug = if slug.is_empty() { "calendar".to_string() } else { slug };
        let base = home.trim_end_matches('/');
        // An address nothing is at yet.
        let mut url = format!("{base}/{slug}/");
        for n in 2..50 {
            let r = self.request(
                "PROPFIND",
                &url,
                Some("0"),
                &[],
                "<?xml version=\"1.0\"?><d:propfind xmlns:d=\"DAV:\"><d:prop><d:resourcetype/></d:prop></d:propfind>",
            )?;
            if r.status == 404 {
                break;
            }
            url = format!("{base}/{slug}-{n}/");
        }
        let colour = colour.map(|c| format!("<ic:calendar-color>{}</ic:calendar-color>", xml_escape(c))).unwrap_or_default();
        let props = format!(
            "<d:displayname>{}</d:displayname>{colour}<c:supported-calendar-component-set><c:comp name=\"VEVENT\"/></c:supported-calendar-component-set>",
            xml_escape(name)
        );
        let ns = "xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\" xmlns:ic=\"http://apple.com/ns/ical/\"";
        let body = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><c:mkcalendar {ns}><d:set><d:prop>{props}</d:prop></d:set></c:mkcalendar>");
        let r = self.request("MKCALENDAR", &url, None, &[], &body)?;
        if (200..300).contains(&r.status) {
            return Ok(url);
        }
        if matches!(r.status, 401 | 403) {
            return Err(Error::Login(format!("{url} answered {} (not allowed to make calendars here)", r.status)));
        }
        // RFC 5689: MKCOL with the resource type.
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><d:mkcol {ns}><d:set><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype>{props}</d:prop></d:set></d:mkcol>"
        );
        let r2 = self.request("MKCOL", &url, None, &[], &body)?;
        if (200..300).contains(&r2.status) {
            return Ok(url);
        }
        Err(Error::Calendar(format!("the server would not make the calendar (MKCALENDAR {}, MKCOL {})", r.status, r2.status)))
    }

    pub fn delete(&self, url: &str, etag: Option<&str>) -> Result<(), Error> {
        let headers: Vec<(&str, &str)> = etag.map(|e| vec![("If-Match", e)]).unwrap_or_default();
        let r = self.request("DELETE", url, None, &headers, "")?;
        if r.status == 404 {
            return Ok(());
        }
        if r.status == 412 {
            return Err(Error::Calendar("the event was changed elsewhere; refresh and try again".into()));
        }
        check(&r, url)
    }
}

fn check(r: &Reply, url: &str) -> Result<(), Error> {
    match r.status {
        200..=299 => Ok(()),
        401 | 403 => Err(Error::Login(format!("{url} answered {}{}", r.status, server_says(&r.body)))),
        s => {
            // Poste.io answers this way while DAV is switched off in its settings.
            let hint =
                r.body.lines().map(str::trim).find(|l| l.to_ascii_lowercase().contains("enable dav")).map(|l| format!(" ({l})")).unwrap_or_default();
            Err(Error::Calendar(format!("{url} answered {s}{hint}")))
        }
    }
}

/// What a refusal says, briefly: Google's JSON `error.message`, or the text of a short
/// HTML/XML/plain body. Empty when there is nothing useful.
fn server_says(body: &str) -> String {
    let msg = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.get("message").or(Some(e))).and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or_else(|| {
            let mut text = String::new();
            let mut in_tag = false;
            for c in body.chars() {
                match c {
                    '<' => in_tag = true,
                    '>' => {
                        in_tag = false;
                        text.push(' ');
                    }
                    _ if !in_tag => text.push(c),
                    _ => {}
                }
            }
            text.split_whitespace().collect::<Vec<_>>().join(" ")
        });
    let msg: String = msg.trim().chars().take(300).collect();
    if msg.is_empty() { String::new() } else { format!(": {msg}") }
}

/// Whether two addresses are the same resource (servers escape characters differently: "%40"
/// for "@", and some give the whole address where others give the path).
pub fn same_url(a: &str, b: &str) -> bool {
    fn decode(s: &str) -> Vec<u8> {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            match (b.get(i), b.get(i + 1).and_then(|c| hex(*c)), b.get(i + 2).and_then(|c| hex(*c))) {
                (Some(b'%'), Some(h), Some(l)) => {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                }
                (Some(c), _, _) => {
                    out.push(*c);
                    i += 1;
                }
                _ => break,
            }
        }
        out
    }
    decode(path_of(a)) == decode(path_of(b))
}

/// A new event's address in a calendar.
pub fn new_url(calendar: &str, uid: &str) -> String {
    // Only characters no server escapes, so the address it lists is the one written.
    let safe: String = uid.chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect();
    format!("{}/{safe}.ics", calendar.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    #[test]
    fn says_why_it_was_refused() {
        let google = r#"{"error":{"code":403,"message":"CalDAV API has not been used in project 785 before or it is disabled.","status":"PERMISSION_DENIED"}}"#;
        assert_eq!(server_says(google), ": CalDAV API has not been used in project 785 before or it is disabled.");
        assert_eq!(server_says("<html><body><h1>Forbidden</h1>\n<p>No access</p></body></html>"), ": Forbidden No access");
        assert_eq!(server_says(""), "");
    }

    use super::*;

    /// Against a real server: MAILCRAFT_CALDAV_TEST=http://host:5232/ (user test, password
    /// testpass, one calendar). `cargo test -p mailcraft-core live_caldav -- --ignored`
    #[test]
    #[ignore]
    fn live_caldav() {
        let Ok(server) = std::env::var("MAILCRAFT_CALDAV_TEST") else { return };
        let mut c = Client::new(Auth::Basic { user: "test".into(), pass: "testpass".into() });
        assert!(matches!(c.discover(&server), Err(Error::Insecure(_))), "plain http refused by default");
        c.insecure_ok = true;
        let home = c.discover(&server).unwrap();
        let cals = c.calendars(&home).unwrap();
        let cal = cals.first().expect("a calendar");
        assert!(!cal.read_only);
        let uid = format!("live-{}@x", std::process::id());
        let mut e =
            crate::ical::Event { uid: uid.clone(), summary: "Live test".into(), rrule: Some("FREQ=DAILY;COUNT=3".into()), ..Default::default() };
        let url = new_url(&cal.url, &uid);
        let etag = c.put(&url, &crate::ical::to_ics(&e, &[]), None).unwrap();
        assert!(c.put(&url, &crate::ical::to_ics(&e, &[]), None).is_err(), "no overwrite of a new one");
        let tags = c.etags(&cal.url).unwrap();
        let mine = tags.iter().find(|t| same_url(&t.url, &url)).expect("listed");
        assert!(etag.as_ref().is_none_or(|t| *t == mine.etag));
        let got = c.fetch(&cal.url, std::slice::from_ref(&url)).unwrap();
        let back = crate::ical::parse(&got[0].data).remove(0);
        assert_eq!((back.summary.as_str(), back.rrule.as_deref()), ("Live test", Some("FREQ=DAILY;COUNT=3")));
        e.summary = "Changed".into();
        assert!(c.put(&url, &crate::ical::to_ics(&e, &[]), Some("\"stale\"")).is_err(), "stale etag refused");
        c.put(&url, &crate::ical::to_ics(&e, &[]), Some(&mine.etag)).unwrap();
        let after = c.calendars(&home).unwrap();
        assert_ne!(after[0].ctag, cal.ctag, "ctag moves on a change");
        // A new calendar, twice with one name: the second gets its own address.
        let made = c.make_calendar(&home, "Live test", Some("#AA3300")).unwrap();
        let made2 = c.make_calendar(&home, "Live test", None).unwrap();
        assert_ne!(made, made2);
        let listed = c.calendars(&home).unwrap();
        let mine = listed.iter().find(|x| same_url(&x.url, &made)).expect("new calendar listed");
        assert_eq!((mine.name.as_str(), mine.colour.as_deref(), mine.read_only), ("Live test", Some("#AA3300"), false));
        c.delete(&made, None).unwrap();
        c.delete(&made2, None).unwrap();
        c.delete(&url, None).unwrap();
        assert!(!c.etags(&cal.url).unwrap().iter().any(|t| same_url(&t.url, &url)));
    }

    const LIST: &str = r##"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:x1="http://apple.com/ns/ical/">
 <d:response><d:href>/remote.php/dav/calendars/mark/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><d:displayname/><x1:calendar-color/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/calendars/mark/personal/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/><cal:calendar/></d:resourcetype><d:displayname>Personal &amp; home</d:displayname>
   <x1:calendar-color>#0082c9</x1:calendar-color><cs:getctag>http://sabre.io/ns/sync/12</cs:getctag>
   <cal:supported-calendar-component-set><cal:comp name="VEVENT"/></cal:supported-calendar-component-set>
   <d:current-user-privilege-set><d:privilege><d:read/></d:privilege><d:privilege><d:write/></d:privilege></d:current-user-privilege-set>
  </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/calendars/mark/tasks/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/><cal:calendar/></d:resourcetype><d:displayname>Tasks</d:displayname>
   <cal:supported-calendar-component-set><cal:comp name="VTODO"/></cal:supported-calendar-component-set>
  </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/calendars/mark/birthdays/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/><cal:calendar/></d:resourcetype><d:displayname>Birthdays</d:displayname>
   <d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set>
  </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
</d:multistatus>"##;

    #[test]
    fn reads_calendar_list() {
        let r = parse_multistatus(LIST).unwrap();
        assert_eq!(r.len(), 4);
        assert!(r[0].prop("displayname").is_none(), "404 props dropped");
        let p = &r[1];
        assert_eq!(p.prop("displayname").map(|x| x.text.as_str()), Some("Personal & home"));
        assert_eq!(p.prop("supported-calendar-component-set").map(|x| x.comps.clone()), Some(vec!["VEVENT".to_string()]));
        assert!(p.prop("current-user-privilege-set").is_some_and(|x| x.inner.iter().any(|n| n == "write")));
    }

    #[test]
    fn calendar_data_keeps_its_lines() {
        let body = "<d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><d:response><d:href>/c/a.ics</d:href><d:propstat><d:prop>\
<d:getetag>&quot;e1&quot;</d:getetag><c:calendar-data>BEGIN:VCALENDAR&#13;\nSUMMARY:Fish &amp; chips&#13;\nEND:VCALENDAR&#13;\n</c:calendar-data>\
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>";
        let r = parse_multistatus(body).unwrap();
        assert_eq!(r[0].prop("getetag").map(|x| x.text.as_str()), Some("\"e1\""));
        assert_eq!(r[0].prop("calendar-data").map(|x| x.text.as_str()), Some("BEGIN:VCALENDAR\r\nSUMMARY:Fish & chips\r\nEND:VCALENDAR\r\n"));
    }

    #[test]
    fn joins_addresses() {
        assert_eq!(join("https://nc.example/remote.php/dav/", "/remote.php/dav/calendars/m/"), "https://nc.example/remote.php/dav/calendars/m/");
        assert_eq!(join("https://h:8443/dav/cal/", "a.ics"), "https://h:8443/dav/cal/a.ics");
        assert_eq!(join("https://h/x", "https://o/y"), "https://o/y");
        assert_eq!(path_of("https://h:5232/mark/cal/"), "/mark/cal/");
        assert_eq!(new_url("https://h/cal/", "a b/c@x"), "https://h/cal/a_b_c_x.ics");
        assert!(same_url("https://h/cal/a%40b.ics", "/cal/a@b.ics"));
        assert!(!same_url("https://h/cal/a.ics", "https://h/cal/b.ics"));
    }
}
