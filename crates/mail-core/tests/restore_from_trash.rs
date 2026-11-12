//! Restore from Trash against a live test server (MAILCRAFT_TEST_IMAP=host:port, user "test",
//! password "testpass", plain IMAP, as GreenMail is set up for the GUI tests): a message
//! deleted from a folder is known in Trash by its Message-ID under its new UID, and goes back
//! to that folder, as MailCraft's app and worker do it.

#[test]
#[ignore = "needs a test IMAP server: MAILCRAFT_TEST_IMAP=host:port"]
fn deleted_mail_goes_back_where_it_came_from() {
    use mailcraft_core::manage;
    use mailcraft_core::sync::sync_folders;
    let Ok(at) = std::env::var("MAILCRAFT_TEST_IMAP") else { return };
    let (host, port) = at.rsplit_once(':').unwrap();
    let ep = mailcraft_core::Endpoint {
        host: host.into(),
        port: port.parse().unwrap(),
        security: mailcraft_autoconfig::Security::None,
        allow_insecure: true,
    };
    let me = "test@mailcraft.test";
    let folder = "RestoreTest";
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let store = mailcraft_core::Store::open(":memory:").unwrap();
    let mut s = rt.block_on(mailcraft_core::connect(&ep, "test", "testpass")).unwrap();
    let _ = rt.block_on(manage::create_folder(&mut s, folder));
    let _ = rt.block_on(s.create("Trash"));
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let mid = format!("<restore-{stamp}@mailcraft.test>");
    let raw = format!(
        "From: Shop <shop@example.com>\r\nTo: {me}\r\nSubject: Restore test {stamp}\r\nDate: Sat, 31 Oct 2026 10:00:00 +0000\r\nMessage-ID: {mid}\r\n\r\nBody.\r\n"
    );
    rt.block_on(mailcraft_core::send::append(&mut s, folder, raw.as_bytes())).unwrap();
    rt.block_on(mailcraft_core::sync::sync_account(&mut s, &store, me)).unwrap();
    let find = |name: &str| {
        let id = store.folder_id(me, name).unwrap().unwrap();
        let uid = store.messages(id, 1000).unwrap().into_iter().find(|m| m.message_id.as_deref().is_some_and(|i| mid.contains(i))).map(|m| m.uid);
        (id, uid)
    };
    let (from_id, Some(uid)) = find(folder) else { panic!("the test message did not arrive in {folder}") };

    // Delete, as the app does: note where from, take it off this PC's copy, then the server.
    assert_eq!(store.remember_origin(from_id, &[uid]).unwrap(), 1);
    store.remove(from_id, &[uid]).unwrap();
    let gone = rt.block_on(manage::delete_messages(&mut s, &store, me, folder, &[uid])).unwrap();
    let manage::Deleted::MovedTo(trash) = gone else { panic!("not moved to Trash: {gone:?}") };
    rt.block_on(sync_folders(&mut s, &store, me, &[folder.to_string(), trash.clone()])).unwrap();
    assert_eq!(find(folder).1, None, "gone from {folder}");
    let (trash_id, Some(in_trash)) = find(&trash) else { panic!("not in {trash}") };
    let o = store.origins(trash_id, &[in_trash]).unwrap();
    assert_eq!(o.first().and_then(|o| o.folder.as_deref()), Some(folder), "known in Trash by its Message-ID: {o:?}");

    // Restore: take it out of Trash here, move it back on the server, forget the record.
    store.remove(trash_id, &[in_trash]).unwrap();
    rt.block_on(manage::move_messages(&mut s, &trash, &[in_trash], folder)).unwrap();
    let ids: Vec<String> = o.iter().filter_map(|o| o.message_id.clone()).collect();
    assert_eq!(store.forget_origins(me, &ids).unwrap(), 1);
    rt.block_on(sync_folders(&mut s, &store, me, &[folder.to_string(), trash.clone()])).unwrap();
    let (back_id, Some(back_uid)) = find(folder) else { panic!("not back in {folder}") };
    assert_eq!(find(&trash).1, None, "not in {trash} any more");
    assert_eq!(store.origins(back_id, &[back_uid]).unwrap().first().and_then(|o| o.folder.clone()), None, "record cleaned up");

    // Tidy the test mailbox.
    rt.block_on(manage::delete_folder(&mut s, folder)).unwrap();
    let _ = rt.block_on(s.logout());
}
