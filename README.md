# MailCraft

An Outlook-style mail client for Windows (and Linux), written in Rust. Every message is cached on
your PC, so moving, deleting and searching happen at once and the server catches up behind you.

Downloads, with checksums: **<https://altcraft.perkin.me.uk/mailcraft/>**

## What it does

- Any IMAP/SMTP account, set up from your address where the provider publishes its settings
  (provider autoconfig, `.well-known`, then the Mozilla ISPDB).
- Gmail and Outlook.com sign-in (OAuth, in your own browser). The downloads have Google sign-in
  built in; Outlook.com, and builds from source, use an app registration of your own, entered in
  MailCraft.
- Folders, a three-pane layout, conversations, a reading pane for HTML mail (in the system's own
  browser engine, locked down, with remote pictures blocked until you ask), compose, reply and
  forward with attachments, drafts, rules, categories, search.
- Calendars: your mail server's CalDAV, Google and Microsoft 365, with invitations.
- Keyboard first: the shortcuts you know from Outlook, F5 to refresh, Ctrl+Z to undo a delete.

## Built on WordCraft

MailCraft's message view and editor are [ArtCraft](https://getartcraft.com/apps)'s **WordCraft**
engine: the document model, layout, rendering and HTML import/export from
[github.com/storytold/wordcraft](https://github.com/storytold/wordcraft), which the ArtCraft team
publish under MIT or Apache-2.0. MailCraft would have taken years longer without it: thank you.

This repository keeps WordCraft's crates and apps alongside MailCraft's own (`apps/mailcraft`,
`apps/mailcraft-cli`, `crates/mail-*`); WordCraft's own README is in
[its repository](https://github.com/storytold/wordcraft). MailCraft is based on WordCraft by the
ArtCraft team, and is not made, sponsored or endorsed by them.

## Build

You need Rust (stable) and, on Linux, the WebKitGTK and GTK development packages.

```sh
cargo run --release -p mailcraft          # the mail client
cargo run --release -p mailcraft-cli -- autoconfig you@example.com
cargo test --workspace
```

Windows builds cross-compile with the `x86_64-pc-windows-gnu` target; the installer
(`packaging/windows/`) is a WiX bundle. On Windows MailCraft needs Microsoft Edge WebView2, which
Windows 10 and 11 already have.

No app registrations are in this repository. To build one in, set `MAILCRAFT_OAUTH_FILE` to an
`oauth.json` (`{"google": {"client_id": "...", "client_secret": "..."}}`) kept outside the tree;
without it, MailCraft asks for the person's own registration when they first sign in.

## Licence

MIT or Apache-2.0, at your option, like the WordCraft code it is built from: see `LICENSE-MIT`,
`LICENSE-APACHE`, `NOTICE` and `ATTRIBUTION.md`. The ArtCraft name and logos are ArtCraft's
trademarks, are not covered by that licence, and are not included here.
