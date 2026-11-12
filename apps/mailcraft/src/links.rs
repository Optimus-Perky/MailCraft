//! Where links go, shown at the bottom of the message while the mouse is over one (as a
//! browser does), so a scam link can be seen before it is clicked. Done in the message itself:
//! each link is labelled with its address, and a style sheet shows the label on hover. A link
//! whose text looks like one website but goes to another is marked, and shown as a warning.

use crate::darkmail::{read_tag, set_attr, write_tag};

/// The website a link goes to ("www." taken off), if it is a web link.
pub fn host(url: &str) -> Option<String> {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")).or_else(|| lower.strip_prefix("//"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // user:password@host:port
    let h = authority.rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("").trim_end_matches('.');
    (!h.is_empty()).then(|| h.trim_start_matches("www.").to_string())
}

/// The website a link's text names, when the text is an address ("paypal.com",
/// "https://www.paypal.com/login", "www.bank.co.uk").
fn text_host(text: &str) -> Option<String> {
    let t = text.trim().trim_end_matches(['/', '.', ',', ')']).to_ascii_lowercase();
    if t.is_empty() || t.contains(char::is_whitespace) || t.contains('@') {
        return None;
    }
    if let Some(h) = host(&t) {
        return Some(h);
    }
    let bare = t.split(['/', '?', '#']).next().unwrap_or("");
    let labels: Vec<&str> = bare.split('.').collect();
    let tld = labels.last().copied().unwrap_or("");
    (labels.len() >= 2 && labels.iter().all(|l| !l.is_empty()) && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()))
        .then(|| bare.trim_start_matches("www.").to_string())
}

/// Do two hosts belong to the same site? The same host, or one inside the other
/// (mail.example.com and example.com); "paypal.com.evil.example" is not paypal.com.
fn same_site(a: &str, b: &str) -> bool {
    a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}"))
}

fn plain_text(html: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in html.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            c if !tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&nbsp;", " ").replace("&#46;", ".")
}

/// The style that shows the labels: a bar at the bottom left, as a browser's.
pub const CSS: &str = "a[data-mc-go]:hover::after{content:\"Goes to \" attr(data-mc-go);position:fixed;left:6px;bottom:6px;\
max-width:calc(100% - 28px);overflow:hidden;text-overflow:ellipsis;white-space:nowrap;box-sizing:border-box;\
padding:4px 9px;font:12px 'Segoe UI',Arial,sans-serif;text-decoration:none;letter-spacing:0;text-transform:none;\
background:#f3f3f3;color:#1f1f1f;border:1px solid #b9b9b9;border-radius:4px;z-index:2147483647;pointer-events:none;\
box-shadow:0 2px 8px rgba(0,0,0,.25);filter:none}\
a[data-mc-warn]:hover::after{content:attr(data-mc-warn);background:#fde7e9;color:#a4262c;border-color:#a4262c;font-weight:600}";

/// Label every link with where it goes.
pub fn label(html: &str) -> String {
    let mut out = String::with_capacity(html.len() + 512);
    let mut i = 0;
    while i < html.len() {
        let rest = html.get(i..).unwrap_or("");
        let Some(at) = rest.to_ascii_lowercase().find("<a") else {
            out.push_str(rest);
            break;
        };
        out.push_str(rest.get(..at).unwrap_or(""));
        i += at;
        let rest = html.get(i..).unwrap_or("");
        let is_a = rest.as_bytes().get(2).is_some_and(|c| c.is_ascii_whitespace() || *c == b'>');
        match read_tag(rest).filter(|_| is_a) {
            Some((mut tag, len)) => {
                let href = tag.attrs.iter().find(|(k, _)| k == "href").and_then(|(_, v)| v.clone()).unwrap_or_default();
                let href = href.trim().replace("&amp;", "&");
                if !href.is_empty() && !href.starts_with('#') {
                    let after = html.get(i + len..).unwrap_or("");
                    let text = plain_text(after.get(..after.to_ascii_lowercase().find("</a").unwrap_or(0)).unwrap_or(""));
                    let go = match host(&href) {
                        Some(h) => format!("{h}  \u{2014}  {href}"),
                        None => href.clone(),
                    };
                    set_attr(&mut tag.attrs, "data-mc-go", go);
                    if let (Some(shown), Some(real)) = (text_host(&text), host(&href))
                        && !same_site(&shown, &real)
                    {
                        set_attr(&mut tag.attrs, "data-mc-warn", format!("Careful: the text says {shown}, but this goes to {real}"));
                    }
                }
                write_tag(&tag, &mut out);
                i += len;
            }
            None => {
                out.push('<');
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        assert_eq!(host("https://www.PayPal.com/signin?x=1").as_deref(), Some("paypal.com"));
        assert_eq!(host("http://user:pw@evil.example:8080/x").as_deref(), Some("evil.example"));
        assert_eq!(host("mailto:a@b.example"), None);
        assert_eq!(text_host("www.bank.co.uk").as_deref(), Some("bank.co.uk"));
        assert_eq!(text_host("Click here"), None);
        assert_eq!(text_host("v1.2"), None);
        assert!(same_site("mail.example.com", "example.com"));
        assert!(!same_site("paypal.com.evil.example", "paypal.com"));
    }

    #[test]
    fn labels_and_warnings() {
        let out = label(
            r##"<p><a href="https://track.shop.example/c?u=1&amp;v=2">Shop now</a> <A HREF="https://paypal.com.secure-login.example/x">https://www.paypal.com</A> <a href="https://www.paypal.com/x">paypal.com</a> <a href="#top">Top</a> <abbr>x</abbr></p>"##,
        );
        assert!(out.contains(r#"data-mc-go="track.shop.example  —  https://track.shop.example/c?u=1&v=2""#), "{out}");
        assert!(out.contains("Careful: the text says paypal.com, but this goes to paypal.com.secure-login.example"), "{out}");
        assert_eq!(out.matches("data-mc-warn").count(), 1, "{out}");
        assert!(out.contains(r##"<a href="#top">Top</a>"##) && out.contains("<abbr>x</abbr>"), "{out}");
    }
}
