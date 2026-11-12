//! A connection kept between requests stays usable: the sequence MailCraft's worker runs on
//! one kept connection, against a live test server (MAILCRAFT_TEST_IMAP=host:port, user
//! "test", password "testpass", plain IMAP, as GreenMail is set up for the GUI tests).

use std::time::Duration;

#[test]
#[ignore = "needs a test IMAP server: MAILCRAFT_TEST_IMAP=host:port"]
fn a_kept_connection_survives_sync_and_flagging() {
    let Ok(at) = std::env::var("MAILCRAFT_TEST_IMAP") else { return };
    let (host, port) = at.rsplit_once(':').unwrap();
    let ep = mailcraft_core::Endpoint {
        host: host.into(),
        port: port.parse().unwrap(),
        security: mailcraft_autoconfig::Security::None,
        allow_insecure: true,
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let store = mailcraft_core::Store::open(":memory:").unwrap();
    let mut s = rt.block_on(mailcraft_core::connect(&ep, "test", "testpass")).unwrap();
    let rep = rt.block_on(mailcraft_core::sync::sync_account(&mut s, &store, "test@mailcraft.test")).unwrap();
    eprintln!("synced {} folders", rep.folders);
    std::thread::sleep(Duration::from_secs(2));
    rt.block_on(s.noop()).expect("NOOP after the sync");
    let inbox = store.folder_id("test@mailcraft.test", "INBOX").unwrap().unwrap();
    let uid = store.messages(inbox, 1).unwrap().first().map(|m| m.uid).expect("a message in INBOX");
    rt.block_on(mailcraft_core::send::store_flags(&mut s, "INBOX", uid, "-FLAGS", "\\Seen")).expect("flag a real message");
    rt.block_on(mailcraft_core::send::store_flags(&mut s, "INBOX", uid, "+FLAGS", "\\Seen")).expect("flag a real message");
    std::thread::sleep(Duration::from_secs(3));
    rt.block_on(s.noop()).expect("NOOP after flagging");
    let rep = rt.block_on(mailcraft_core::sync::sync_folders(&mut s, &store, "test@mailcraft.test", &["INBOX".into()])).unwrap();
    eprintln!("refreshed {} folder", rep.folders);
    std::thread::sleep(Duration::from_secs(3));
    rt.block_on(s.noop()).expect("NOOP after a folder refresh");
}
