//! MailCraft command line.
//!
//!   mailcraft-cli autoconfig <email>                               find the account's servers
//!   mailcraft-cli sync   <host> <port> <tls|starttls|plain> <user> <cache.db>
//!   mailcraft-cli folders <cache.db> <user>
//!   mailcraft-cli list   <cache.db> <user> <folder>
//!   mailcraft-cli open   <host> <port> <tls|starttls|plain> <user> <cache.db> <folder> <uid>
//!
//! The password comes from the MAILCRAFT_PASSWORD environment variable, never the command line.
//! "plain" (no encryption) is for local test servers only.

use mailcraft_autoconfig::Security;
use mailcraft_core::{Endpoint, Store};

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

fn endpoint(host: &str, port: &str, sec: &str) -> Endpoint {
    let security = match sec {
        "tls" => Security::Tls,
        "starttls" => Security::StartTls,
        "plain" => Security::None,
        other => die(format!("security must be tls, starttls or plain, not {other}")),
    };
    Endpoint { host: host.into(), port: port.parse().unwrap_or_else(|_| die("bad port")), security, allow_insecure: sec == "plain" }
}

fn password() -> String {
    std::env::var("MAILCRAFT_PASSWORD").unwrap_or_else(|_| die("set MAILCRAFT_PASSWORD"))
}

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize| a.get(i).map(String::as_str).unwrap_or_else(|| die("missing argument; see the usage at the top of main.rs"));
    match a.get(1).map(String::as_str) {
        Some("autoconfig") => match mailcraft_autoconfig::discover(arg(2)).await {
            Ok(s) => println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default()),
            Err(e) => die(e),
        },
        Some("sync") => {
            let store = Store::open(arg(6)).unwrap_or_else(|e| die(e));
            let mut s = mailcraft_core::connect(&endpoint(arg(2), arg(3), arg(4)), arg(5), &password()).await.unwrap_or_else(|e| die(e));
            let r = mailcraft_core::sync::sync_account(&mut s, &store, arg(5)).await.unwrap_or_else(|e| die(e));
            let _ = s.logout().await;
            println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
        }
        Some("folders") => {
            let store = Store::open(arg(2)).unwrap_or_else(|e| die(e));
            for f in store.folders(arg(3)).unwrap_or_else(|e| die(e)) {
                println!("{:<30} {:<10} {:>5} msgs {:>5} unread", f.name, f.special.unwrap_or_default(), f.total, f.unread);
            }
        }
        Some("list") => {
            let store = Store::open(arg(2)).unwrap_or_else(|e| die(e));
            let id = store.folder_id(arg(3), arg(4)).unwrap_or_else(|e| die(e)).unwrap_or_else(|| die("no such folder; sync first"));
            for m in store.messages(id, 50).unwrap_or_else(|e| die(e)) {
                let from = m.from_name.clone().or(m.from_addr.clone()).unwrap_or_default();
                println!(
                    "{:>6} {} {:<25.25} {:<40.40} {}",
                    m.uid,
                    if m.seen() { " " } else { "*" },
                    from,
                    m.subject.clone().unwrap_or_default(),
                    m.date.clone().unwrap_or_default()
                );
            }
        }
        Some("open") => {
            let store = Store::open(arg(6)).unwrap_or_else(|e| die(e));
            let mut s = mailcraft_core::connect(&endpoint(arg(2), arg(3), arg(4)), arg(5), &password()).await.unwrap_or_else(|e| die(e));
            let uid: u32 = arg(8).parse().unwrap_or_else(|_| die("bad uid"));
            let raw = mailcraft_core::sync::fetch_body(&mut s, &store, arg(5), arg(7), uid).await.unwrap_or_else(|e| die(e));
            let _ = s.logout().await;
            let m = mail_parser::MessageParser::default().parse(&raw).unwrap_or_else(|| die("could not parse the message"));
            println!("Subject: {}", m.subject().unwrap_or_default());
            println!("From:    {}", m.from().and_then(|a| a.first()).and_then(|a| a.address()).unwrap_or_default());
            println!(
                "HTML:    {} bytes, text: {} bytes, attachments: {}",
                m.body_html(0).map(|b| b.len()).unwrap_or(0),
                m.body_text(0).map(|b| b.len()).unwrap_or(0),
                m.attachment_count()
            );
            println!("---\n{}", m.body_text(0).unwrap_or_default());
        }
        _ => die("usage: see the top of apps/mailcraft-cli/src/main.rs"),
    }
}
