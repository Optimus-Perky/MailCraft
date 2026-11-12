//! mailto: links (RFC 6068), for MailCraft as the system's email handler, and one MailCraft
//! window per user.
//!
//! Windows starts `mailcraft.exe "mailto:..."` for every email link clicked. When MailCraft is
//! already open, that process hands the link to it over a loopback socket and exits, so the
//! link opens a new message in the window that is already there rather than in a second copy
//! of the app. Starting MailCraft from its icon does the same with [`SHOW`]: the open window
//! comes forward. The window must answer within a few seconds, so one that has hung does not
//! stop a new copy starting (`--new-window` starts one regardless). The socket's port and a random token are in %LOCALAPPDATA%\MailCraft\instance;
//! a connection without the token is ignored. A link only ever fills in a new message: nothing
//! is sent until the user presses Send.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// What a launch without a link sends: bring the window forward.
pub const SHOW: &str = "show";

/// How long the open window has to say it got the message.
const ANSWER: Duration = Duration::from_secs(4);

/// Longer than Windows lets a command line be, so never in the way of a real link.
const MAX_LINK: usize = 64 * 1024;

#[derive(Debug, Default, PartialEq)]
pub struct Mailto {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
}

/// Whether a command-line argument is an email link.
pub fn is_link(arg: &str) -> bool {
    arg.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("mailto:"))
}

/// The parts of a mailto: link. Addresses may be in the path and in to=/cc=/bcc= fields; `+`
/// is a plus sign, not a space (RFC 6068 2); other fields (in-reply-to, ...) are ignored.
pub fn parse(link: &str) -> Option<Mailto> {
    if !is_link(link) || link.len() > MAX_LINK {
        return None;
    }
    let rest = link.get(7..)?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut m = Mailto::default();
    add_addresses(&mut m.to, &decode(path));
    for field in query.split('&') {
        let (key, value) = field.split_once('=').unwrap_or((field, ""));
        let value = decode(value);
        match decode(key).to_ascii_lowercase().as_str() {
            "to" => add_addresses(&mut m.to, &value),
            "cc" => add_addresses(&mut m.cc, &value),
            "bcc" => add_addresses(&mut m.bcc, &value),
            "subject" => m.subject = value.replace(['\r', '\n'], " "),
            "body" => m.body = value.replace("\r\n", "\n").replace('\r', "\n"),
            _ => {}
        }
    }
    Some(m)
}

fn add_addresses(list: &mut Vec<String>, s: &str) {
    for a in s.split([',', ';']).map(str::trim).filter(|a| !a.is_empty()) {
        if !list.iter().any(|b| b.eq_ignore_ascii_case(a)) {
            list.push(a.to_string());
        }
    }
}

/// Percent-decoding as UTF-8; a % not followed by two hex digits stays as it is.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%'
            && let (Some(h), Some(l)) = (b.get(i + 1).and_then(hex), b.get(i + 2).and_then(hex))
        {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: &u8) -> Option<u8> {
    (*c as char).to_digit(16).and_then(|d| u8::try_from(d).ok())
}

/// The body as the HTML a new message starts with: one paragraph per blank-line-separated
/// block, line breaks kept.
pub fn body_html(body: &str) -> String {
    body.split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|p| format!("<p>{}</p>", crate::escape_html(p.trim_end()).replace('\n', "<br>")))
        .collect()
}

fn instance_file() -> std::path::PathBuf {
    crate::accounts::cache_path().with_file_name("instance")
}

/// Give a link (or [`SHOW`]) to a MailCraft that is already running. False when none answers
/// (none is open, the last one did not close cleanly, or it has hung), and this process should
/// open the window.
pub fn hand_to_running(link: &str) -> bool {
    let Ok(s) = std::fs::read_to_string(instance_file()) else { return false };
    let mut parts = s.split_whitespace();
    let (Some(port), Some(token)) = (parts.next().and_then(|p| p.parse::<u16>().ok()), parts.next()) else {
        return false;
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut c) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else { return false };
    let _ = c.set_read_timeout(Some(ANSWER + Duration::from_secs(2)));
    let link = link.replace(['\r', '\n'], "");
    if c.write_all(format!("{token}\n{link}\n").as_bytes()).is_err() {
        return false;
    }
    let mut answer = String::new();
    BufReader::new(c).read_line(&mut answer).is_ok() && answer.trim() == "ok"
}

/// A link or [`SHOW`] from a later launch, and where to say it arrived.
pub type Message = (String, Sender<()>);

/// Listen for later launches. Each message arrives on the receiver and wakes the window, which
/// answers on the message's sender; the launch is told "ok" only once it has.
pub fn listen(ctx: egui::Context) -> Option<Receiver<Message>> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    let mut raw = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut raw).ok()?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(instance_file(), format!("{port} {token}\n")).ok()?;
    let _ = TOKEN.set(token.clone());
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("mailto".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
                let Ok(mut reply) = conn.try_clone() else { continue };
                let mut lines = BufReader::new(std::io::Read::take(conn, MAX_LINK as u64 + 64)).lines();
                let (Some(Ok(t)), Some(Ok(link))) = (lines.next(), lines.next()) else { continue };
                if t != token || !(is_link(&link) || link == SHOW) {
                    continue;
                }
                let (done, answered) = channel();
                if tx.send((link, done)).is_err() {
                    return;
                }
                ctx.request_repaint();
                let answer: &[u8] = if answered.recv_timeout(ANSWER).is_ok() { b"ok\n" } else { b"busy\n" };
                let _ = reply.write_all(answer);
            }
        })
        .ok()?;
    Some(rx)
}

static TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Remove the instance file when this window closes, if it is still ours (a copy started
/// later, while this one was busy starting, may have written its own).
pub fn forget() {
    let Some(token) = TOKEN.get() else { return };
    if std::fs::read_to_string(instance_file()).is_ok_and(|s| s.split_whitespace().nth(1) == Some(token.as_str())) {
        let _ = std::fs::remove_file(instance_file());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        assert_eq!(parse("https://example.com"), None);
        let m = parse("mailto:a@example.com").unwrap();
        assert_eq!(m.to, ["a@example.com"]);
        let m = parse(
            "MAILTO:a@example.com,b@example.com?subject=Hello%20there&cc=c@example.com&bcc=d@example.com&body=Line%201%0D%0ALine%202%0D%0A%0D%0ANext",
        )
        .unwrap();
        assert_eq!(m.to, ["a@example.com", "b@example.com"]);
        assert_eq!(m.cc, ["c@example.com"]);
        assert_eq!(m.bcc, ["d@example.com"]);
        assert_eq!(m.subject, "Hello there");
        assert_eq!(m.body, "Line 1\nLine 2\n\nNext");
        assert_eq!(body_html(&m.body), "<p>Line 1<br>Line 2</p><p>Next</p>");
        // No address, only fields; to= adds to the path's; + is a plus; UTF-8; bad % kept.
        let m = parse("mailto:?to=x%2Btag@example.com&subject=Caf%C3%A9+100%&to=y@example.com").unwrap();
        assert_eq!(m.to, ["x+tag@example.com", "y@example.com"]);
        assert_eq!(m.subject, "Café+100%");
        // Header injection through the subject does not survive.
        assert_eq!(parse("mailto:a@b?subject=Hi%0D%0ABcc:%20evil@x").unwrap().subject, "Hi  Bcc: evil@x");
        assert_eq!(body_html("<b>&"), "<p>&lt;b&gt;&amp;</p>");
        assert!(parse(&format!("mailto:a@b?body={}", "x".repeat(MAX_LINK))).is_none());
    }

    #[test]
    fn decode_never_panics() {
        for s in ["%", "%4", "%zz", "%e2%82", "%%41", "\u{e9}%"] {
            let _ = decode(s);
            let _ = parse(&format!("mailto:{s}?{s}={s}&{s}"));
        }
    }
}
