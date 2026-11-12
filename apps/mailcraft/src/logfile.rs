//! MailCraft's log: what it asked the servers, how long each took and what came back, so a
//! "it just stopped" can be diagnosed afterwards (2026-10-29: "I think you should add logs
//! to it, so we can diagnose issues").
//!
//! %LOCALAPPDATA%\MailCraft\logs\mailcraft.log (beside the mail cache), up to 5 MB, then
//! renamed mailcraft.1.log and so on, keeping five. Never a password or token: requests are
//! logged by what they are and which account, not by what they carry. MAILCRAFT_LOG=debug
//! (or trace) logs more, including the libraries' own messages.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_BYTES: u64 = 5 * 1024 * 1024;
const KEEP: usize = 5;

struct FileLog {
    file: Mutex<Option<(File, u64)>>,
    level: log::LevelFilter,
}

/// Where the log files are.
pub fn dir() -> PathBuf {
    crate::accounts::cache_path().parent().map(|p| p.join("logs")).unwrap_or_else(|| PathBuf::from("logs"))
}

fn path(n: usize) -> PathBuf {
    if n == 0 { dir().join("mailcraft.log") } else { dir().join(format!("mailcraft.{n}.log")) }
}

/// mailcraft.log becomes mailcraft.1.log, 1 becomes 2, and the oldest goes.
fn rotate() {
    let _ = std::fs::remove_file(path(KEEP));
    for n in (0..KEEP).rev() {
        let _ = std::fs::rename(path(n), path(n + 1));
    }
}

fn open() -> Option<(File, u64)> {
    let _ = std::fs::create_dir_all(dir());
    let f = OpenOptions::new().create(true).append(true).open(path(0)).ok()?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    Some((f, len))
}

impl log::Log for FileLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        // MailCraft's own at the chosen level; the libraries' (rustls, ureq...) only when they
        // warn, unless more was asked for.
        let ours = m.target().starts_with("mailcraft");
        m.level() <= if ours || self.level > log::LevelFilter::Info { self.level } else { log::LevelFilter::Warn }
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {}: {}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            r.level(),
            r.target().trim_start_matches("mailcraft::").trim_start_matches("mailcraft_core::"),
            r.args()
        );
        let Ok(mut g) = self.file.lock() else { return };
        if g.as_ref().is_some_and(|(_, len)| *len >= MAX_BYTES) {
            *g = None;
            rotate();
        }
        if g.is_none() {
            *g = open();
        }
        if let Some((f, len)) = g.as_mut()
            && f.write_all(line.as_bytes()).is_ok()
        {
            *len += line.len() as u64;
        }
    }

    fn flush(&self) {
        if let Ok(mut g) = self.file.lock()
            && let Some((f, _)) = g.as_mut()
        {
            let _ = f.flush();
        }
    }
}

/// Start logging to the file, and log a panic before it takes MailCraft down.
pub fn init() {
    let level = match std::env::var("MAILCRAFT_LOG").unwrap_or_default().to_ascii_lowercase().as_str() {
        "trace" => log::LevelFilter::Trace,
        "debug" => log::LevelFilter::Debug,
        "warn" => log::LevelFilter::Warn,
        "error" => log::LevelFilter::Error,
        "off" => log::LevelFilter::Off,
        _ => log::LevelFilter::Info,
    };
    // A file grown past its size since the last run starts the new run in a fresh one.
    if std::fs::metadata(path(0)).is_ok_and(|m| m.len() >= MAX_BYTES) {
        rotate();
    }
    let logger = Box::new(FileLog { file: Mutex::new(open()), level });
    if log::set_boxed_logger(logger).is_ok() {
        log::set_max_level(level.max(log::LevelFilter::Warn));
    }
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("MailCraft stopped: {info}");
        log::logger().flush();
        prev(info);
    }));
}
