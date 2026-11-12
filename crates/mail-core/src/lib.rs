//! MailCraft's mail core: connecting to an IMAP server, keeping a local copy of folders and
//! message headers in SQLite, and fetching whole messages when they are opened.
//!
//! Sync is incremental: each folder remembers its UIDVALIDITY and the highest UID it has seen,
//! so a second sync fetches only what arrived since (plus flag changes and deletions).

pub mod caldav;
pub mod calstore;
pub mod connect;
pub mod fulltext;
pub mod graph;
pub mod ical;
pub mod manage;
pub mod oauth;
pub mod outline;
pub mod rules;
pub mod send;
pub mod store;
pub mod sync;
pub mod threads;

pub use connect::{Endpoint, Login, connect, connect_with};
pub use store::{FolderRow, MessageRow, Store};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not reach {0}: {1}")]
    Connect(String, String),
    #[error("TLS: {0}")]
    Tls(String),
    #[error("IMAP: {0}")]
    Imap(String),
    #[error("the server refused the login: {0}")]
    Login(String),
    #[error("local cache: {0}")]
    Store(#[from] rusqlite::Error),
    #[error("sign-in: {0}")]
    OAuth(String),
    #[error("sending: {0}")]
    Send(String),
    #[error("calendar: {0}")]
    Calendar(String),
    #[error("refusing an unencrypted connection to {0}; only allowed when explicitly asked for (test servers)")]
    Insecure(String),
}

impl From<async_imap::error::Error> for Error {
    fn from(e: async_imap::error::Error) -> Self {
        Error::Imap(e.to_string())
    }
}
