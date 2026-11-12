//! What the message list needs without downloading whole messages: the names of a message's
//! attachments and which part holds its text (both from BODYSTRUCTURE), whether it is bulk mail
//! (newsletters, notifications: for the Focused Inbox), and a short preview of its text.

use async_imap::imap_proto::{BodyStructure, ContentEncoding};

/// The part to read a preview from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPart {
    /// IMAP section numbers ("1.2" is [1, 2]).
    pub section: Vec<u32>,
    pub html: bool,
    pub encoding: String,
    pub charset: Option<String>,
    /// Its size in octets, as sent.
    pub size: u32,
}

impl TextPart {
    /// "1.2", as FETCH wants it.
    pub fn section_text(&self) -> String {
        self.section.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outline {
    pub attachments: Vec<String>,
    pub text: Option<TextPart>,
}

fn param<'a>(params: &'a Option<Vec<(std::borrow::Cow<'_, str>, std::borrow::Cow<'_, str>)>>, name: &str) -> Option<&'a str> {
    params.as_ref()?.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_ref())
}

/// A file name as sent: RFC 2047 encoded words ("=?UTF-8?B?...?=") or RFC 2231 ("UTF-8''%E2%82%AC") made readable.
fn readable_name(raw: &str, rfc2231: bool) -> String {
    if rfc2231 {
        let v = raw.split_once("''").map(|(_, v)| v).unwrap_or(raw);
        let mut out = vec![];
        let b = v.as_bytes();
        let mut i = 0;
        while let Some(&c) = b.get(i) {
            let hex = |j: usize| b.get(j).and_then(|h| (*h as char).to_digit(16));
            match (c, hex(i + 1), hex(i + 2)) {
                (b'%', Some(h), Some(l)) => {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                }
                _ => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        return String::from_utf8_lossy(&out).into_owned();
    }
    if raw.contains("=?") {
        let fake = format!("Subject: {raw}\r\n\r\n");
        if let Some(s) = mail_parser::MessageParser::default().parse(fake.as_bytes()).and_then(|m| m.subject().map(str::to_string)) {
            return s;
        }
    }
    raw.to_string()
}

fn encoding_text(e: &ContentEncoding<'_>) -> String {
    match e {
        ContentEncoding::SevenBit => "7bit".into(),
        ContentEncoding::EightBit => "8bit".into(),
        ContentEncoding::Binary => "binary".into(),
        ContentEncoding::Base64 => "base64".into(),
        ContentEncoding::QuotedPrintable => "quoted-printable".into(),
        ContentEncoding::Other(o) => o.to_lowercase(),
    }
}

/// Read a BODYSTRUCTURE.
pub fn outline(bs: &BodyStructure<'_>) -> Outline {
    let mut o = Outline::default();
    let mut plain = None;
    let mut html = None;
    walk(bs, vec![], true, &mut o.attachments, &mut plain, &mut html, 0);
    o.text = plain.or(html);
    o
}

fn walk(
    bs: &BodyStructure<'_>,
    path: Vec<u32>,
    root: bool,
    files: &mut Vec<String>,
    plain: &mut Option<TextPart>,
    html: &mut Option<TextPart>,
    depth: u32,
) {
    if depth > 20 || files.len() > 50 {
        return;
    }
    // A single-part message's body is part 1.
    let here = if root && !matches!(bs, BodyStructure::Multipart { .. }) { vec![1] } else { path.clone() };
    match bs {
        BodyStructure::Multipart { bodies, .. } => {
            for (i, b) in bodies.iter().enumerate() {
                let mut p = path.clone();
                p.push(i as u32 + 1);
                walk(b, p, false, files, plain, html, depth + 1);
            }
        }
        BodyStructure::Text { common, other, .. } | BodyStructure::Basic { common, other, .. } | BodyStructure::Message { common, other, .. } => {
            let disp = common.disposition.as_ref();
            let attached = disp.is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"));
            let star = disp.and_then(|d| param(&d.params, "filename*"));
            let name = star
                .map(|n| readable_name(n, true))
                .or_else(|| disp.and_then(|d| param(&d.params, "filename")).map(|n| readable_name(n, false)))
                .or_else(|| param(&common.ty.params, "name").map(|n| readable_name(n, false)));
            let ty = common.ty.ty.to_ascii_lowercase();
            let sub = common.ty.subtype.to_ascii_lowercase();
            let is_text = matches!(bs, BodyStructure::Text { .. }) && (sub == "plain" || sub == "html");
            if is_text && !attached {
                let part = TextPart {
                    section: here,
                    html: sub == "html",
                    encoding: encoding_text(&other.transfer_encoding),
                    charset: param(&common.ty.params, "charset").map(str::to_string),
                    size: other.octets,
                };
                let slot = if part.html { html } else { plain };
                if slot.is_none() {
                    *slot = Some(part);
                }
                return;
            }
            // Pictures the body shows (inline, with a Content-ID) are part of it, not attachments.
            let body_picture = ty == "image" && other.id.is_some() && !attached;
            if body_picture {
                return;
            }
            if attached || name.is_some() || matches!(bs, BodyStructure::Message { .. }) {
                files.push(
                    name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
                        if matches!(bs, BodyStructure::Message { .. }) { "Attached message".into() } else { format!("{ty}/{sub}") }
                    }),
                );
            }
        }
    }
}

/// Bulk mail by its headers (just the header block is enough): mailing lists, newsletters,
/// automatic notifications.
pub fn is_bulk(header: &[u8]) -> bool {
    let Some(m) = mail_parser::MessageParser::default().parse(header) else { return false };
    let raw = |n: &str| m.header_raw(n).map(|v| v.trim().to_ascii_lowercase());
    if raw("List-Unsubscribe").is_some() || raw("List-Id").is_some() {
        return true;
    }
    if raw("Precedence").is_some_and(|p| p == "bulk" || p == "list" || p == "junk") {
        return true;
    }
    if raw("Auto-Submitted").is_some_and(|a| !a.is_empty() && a != "no") {
        return true;
    }
    let from = m.from().and_then(|a| a.first()).and_then(|a| a.address()).unwrap_or("").to_ascii_lowercase();
    let local = from.split('@').next().unwrap_or("");
    ["noreply", "no-reply", "donotreply", "do-not-reply", "newsletter", "mailer-daemon", "notifications", "notification"]
        .iter()
        .any(|w| local == *w || local.starts_with(&format!("{w}+")) || local.starts_with(&format!("{w}.")))
}

/// The sender's priority from the headers: 1 (highest) to 5 (lowest); None for normal or
/// unset. X-Priority ("1 (Highest)"), else Importance (high/low), else Priority (urgent).
pub fn priority(header: &[u8]) -> Option<u8> {
    let m = mail_parser::MessageParser::default().parse(header)?;
    let raw = |n: &str| m.header_raw(n).map(|v| v.trim().to_ascii_lowercase());
    let p = if let Some(x) = raw("X-Priority") {
        x.chars().next().and_then(|c| c.to_digit(10)).map(|d| d.clamp(1, 5) as u8)
    } else if let Some(i) = raw("Importance") {
        match i.as_str() {
            "high" => Some(1),
            "low" => Some(5),
            _ => None,
        }
    } else {
        match raw("Priority").as_deref() {
            Some("urgent") => Some(1),
            Some("non-urgent") => Some(5),
            _ => None,
        }
    };
    p.filter(|p| *p != 3)
}

/// At most this many characters of preview are kept.
pub const PREVIEW_CHARS: usize = 200;

/// The first words of a text: quoted lines and blank space dropped, cut at a word.
pub fn tidy_preview(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('>') {
            continue;
        }
        // Where a reply quotes the message before it, the rest is old news.
        if l.starts_with("On ") && l.ends_with("wrote:") || l.starts_with("-----Original Message") {
            break;
        }
        for w in l.split_whitespace() {
            if out.chars().count() + w.chars().count() + 1 > PREVIEW_CHARS {
                return out;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(w);
        }
    }
    out
}

/// A preview from the first bytes of a text part (as fetched with BODY.PEEK[n]<0.size>).
pub fn preview_from_part(part: &TextPart, data: &[u8]) -> String {
    // A cut-off base64 line would not decode: stop at the last whole line.
    let data = if part.encoding == "base64" {
        match data.iter().rposition(|&b| b == b'\n') {
            Some(i) => data.get(..i).unwrap_or(data),
            None => data,
        }
    } else {
        data
    };
    let mut raw = format!(
        "Content-Type: text/{}{}\r\nContent-Transfer-Encoding: {}\r\n\r\n",
        if part.html { "html" } else { "plain" },
        part.charset.as_deref().map(|c| format!("; charset=\"{c}\"")).unwrap_or_default(),
        part.encoding
    )
    .into_bytes();
    raw.extend_from_slice(data);
    preview_from_message(&raw)
}

/// A preview from a whole message.
pub fn preview_from_message(raw: &[u8]) -> String {
    let Some(m) = mail_parser::MessageParser::default().parse(raw) else { return String::new() };
    // The plain text when the message has one; else its HTML's words (style sheets, scripts
    // and comments left out: they are not words).
    let has_plain = m.text_body.iter().any(|&i| m.parts.get(i as usize).is_some_and(|p| !p.is_text_html()));
    match m.body_html(0).filter(|_| !has_plain) {
        Some(html) => tidy_preview(&html_words(&html)),
        None => tidy_preview(&m.body_text(0).unwrap_or_default()),
    }
}

/// The words of some HTML: tags, style sheets, scripts, comments and the head dropped; block
/// ends become line breaks; the common entities decoded.
pub fn html_words(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0;
    while i < html.len() {
        let rest = lower.get(i..).unwrap_or("");
        if rest.starts_with("<!--") {
            i += rest.find("-->").map(|k| k + 3).unwrap_or(rest.len());
            continue;
        }
        let skip = ["style", "script", "head", "title"]
            .iter()
            .find(|t| rest.starts_with(&format!("<{t}")) && rest.as_bytes().get(t.len() + 1).is_some_and(|c| !c.is_ascii_alphanumeric()));
        if let Some(t) = skip {
            let close = format!("</{t}");
            i += rest.find(&close).and_then(|k| rest.get(k..).and_then(|r| r.find('>')).map(|e| k + e + 1)).unwrap_or(rest.len());
            continue;
        }
        if rest.starts_with('<') {
            let end = rest.find('>').map(|k| k + 1).unwrap_or(rest.len());
            let tag = rest.get(1..end).unwrap_or("");
            let block = ["p", "/p", "br", "div", "/div", "tr", "/tr", "li", "h1", "h2", "h3", "/h1", "/h2", "/h3", "td", "table"]
                .iter()
                .any(|b| tag.starts_with(b) && tag.as_bytes().get(b.len()).is_none_or(|c| !c.is_ascii_alphanumeric()));
            out.push(if block { '\n' } else { ' ' });
            i += end;
            continue;
        }
        let next = rest.find('<').unwrap_or(rest.len());
        out.push_str(html.get(i..i + next).unwrap_or(""));
        i += next;
    }
    out.replace("&nbsp;", " ")
        .replace("&#160;", " ")
        .replace("&zwnj;", "")
        .replace("&#8204;", "")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&rsquo;", "\u{2019}")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews() {
        assert_eq!(tidy_preview("Hi Mark,\n\n  The strap   fits.\n> old\nOn Thu, Mark wrote:\nquoted"), "Hi Mark, The strap fits.");
        let html = TextPart { section: vec![1], html: true, encoding: "quoted-printable".into(), charset: Some("utf-8".into()), size: 0 };
        let p = preview_from_part(&html, b"<html><head><style>p{color:red}</style></head><body><p>Your parcel =E2=80=93 out for delivery</p>");
        assert!(p.contains("Your parcel \u{2013} out for delivery"), "{p}");
        assert!(!p.contains("color"), "{p}");
        let b64 = TextPart { section: vec![1, 1], html: false, encoding: "base64".into(), charset: None, size: 0 };
        // "Hello there, this is a test." then a cut-off line.
        assert_eq!(preview_from_part(&b64, b"SGVsbG8gdGhlcmUsIHRoaXMgaXMgYSB0ZXN0Lg==\r\nSGVsbG"), "Hello there, this is a test.");
        let long = "word ".repeat(100);
        assert!(tidy_preview(&long).chars().count() <= PREVIEW_CHARS);
    }

    #[test]
    fn html_previews_leave_out_styles() {
        let html = "<html><head><title>T</title><style>html, body { font-family:Helvetica }</style></head><body><style>.x{color:red}</style><!-- hi --><h1>Free treat</h1><p>You deserve it&nbsp;&amp; more</p><script>var a=1;</script></body></html>";
        assert_eq!(tidy_preview(&html_words(html)), "Free treat You deserve it & more");
        let raw = format!("Content-Type: text/html\r\n\r\n{html}");
        assert_eq!(preview_from_message(raw.as_bytes()), "Free treat You deserve it & more");
    }

    #[test]
    fn priorities() {
        assert_eq!(priority(b"X-Priority: 1 (Highest)\r\n\r\n"), Some(1));
        assert_eq!(priority(b"X-Priority: 3\r\n\r\n"), None);
        assert_eq!(priority(b"Importance: Low\r\n\r\n"), Some(5));
        assert_eq!(priority(b"Subject: hi\r\n\r\n"), None);
    }

    #[test]
    fn bulk_mail() {
        assert!(is_bulk(b"From: Shop <news@shop.example>\r\nList-Unsubscribe: <mailto:u@shop.example>\r\n\r\n"));
        assert!(is_bulk(b"From: Bank <no-reply@bank.example>\r\n\r\n"));
        assert!(is_bulk(b"From: a@b.example\r\nAuto-Submitted: auto-generated\r\n\r\n"));
        assert!(!is_bulk(b"From: Mark <mark@example.com>\r\nAuto-Submitted: no\r\n\r\n"));
    }

    #[test]
    fn structures() {
        use async_imap::imap_proto::{AttributeValue, Response};
        let read = |line: &[u8]| {
            let (_, r) = Response::parse(line).unwrap();
            let Response::Fetch(_, attrs) = r else { panic!("not a fetch") };
            attrs.into_iter().find_map(|a| if let AttributeValue::BodyStructure(b) = a { Some(outline(&b)) } else { None }).unwrap()
        };
        // multipart/mixed: (alternative: plain, html), a PDF, an inline picture with a Content-ID.
        let o = read(
            b"* 1 FETCH (UID 7 BODYSTRUCTURE (((\"text\" \"plain\" (\"charset\" \"utf-8\") NIL NIL \"quoted-printable\" 120 4 NIL NIL NIL)\
              (\"text\" \"html\" (\"charset\" \"utf-8\") NIL NIL \"base64\" 900 12 NIL NIL NIL) \"alternative\" (\"boundary\" \"b2\") NIL NIL)\
              (\"application\" \"pdf\" (\"name\" \"invoice.pdf\") NIL NIL \"base64\" 5000 NIL (\"attachment\" (\"filename\" \"invoice.pdf\")) NIL)\
              (\"image\" \"png\" (\"name\" \"logo.png\") \"<logo@x>\" NIL \"base64\" 300 NIL (\"inline\" (\"filename\" \"logo.png\")) NIL) \
              \"mixed\" (\"boundary\" \"b1\") NIL NIL))\r\n",
        );
        assert_eq!(o.attachments, vec!["invoice.pdf".to_string()]);
        let t = o.text.unwrap();
        assert_eq!((t.section_text(), t.html, t.encoding.as_str()), ("1.1".to_string(), false, "quoted-printable"));
        // A plain single-part message: its text is part 1, nothing attached.
        let o = read(b"* 2 FETCH (UID 8 BODYSTRUCTURE (\"text\" \"html\" NIL NIL NIL \"7bit\" 40 2 NIL NIL NIL))\r\n");
        assert!(o.attachments.is_empty());
        assert_eq!(o.text.map(|t| (t.section, t.html)), Some((vec![1], true)));
    }

    #[test]
    fn names() {
        assert_eq!(readable_name("UTF-8''%E2%82%AC%20invoice.pdf", true), "\u{20ac} invoice.pdf");
        assert_eq!(readable_name("=?UTF-8?B?w6l0w6kucGRm?=", false), "\u{e9}t\u{e9}.pdf");
        assert_eq!(readable_name("plain.txt", false), "plain.txt");
    }
}
