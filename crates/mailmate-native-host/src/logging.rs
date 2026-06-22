//! File logging for the native messaging host.
//!
//! Thunderbird launches the host as a native-messaging child, which leaves it with **no usable
//! console**: stdout is the framed protocol channel (anything else on it desyncs the stream) and
//! stderr is discarded by the confined Thunderbird snap. The only durable window into a failure — a
//! stalled `list_models` probe, a rejected request, a panic in a handler — is a file.
//!
//! This installs a [`log`] facade that appends timestamped lines to `<data_dir>/host.log`. Because
//! it is the global `log` logger, it captures both the host's own `log::info!`/`warn!`/`error!`
//! lines *and* the transport crate's (`ureq`) records — so an HTTP-level failure shows up too.
//!
//! Level is `info` by default; `MAILMATE_LOG=debug` (or `trace`/`warn`/`error`/`off`) overrides it.
//! Setup is best-effort: an unwritable data directory degrades to "no log", never a failed launch,
//! and the file is rolled once past [`MAX_BYTES`] so a long-lived session can't grow it without bound.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use log::{LevelFilter, Log, Metadata, Record};
use time::macros::format_description;
use time::OffsetDateTime;

/// The installed logger. Held in a `static` so [`log::set_logger`] gets the `'static` borrow it
/// requires without leaking a `Box`.
static LOGGER: OnceLock<FileLogger> = OnceLock::new();

/// Roll `host.log` to `host.log.old` once it passes this size (a single retained generation).
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// A `log::Log` that appends one formatted line per record to an open file, flushing each write so
/// a crash right after a log call still leaves the line on disk.
struct FileLogger {
    sink: Mutex<File>,
    level: LevelFilter,
}

impl Log for FileLogger {
    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        meta.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {}: {}\n",
            now_ts(),
            record.level(),
            record.target(),
            record.args(),
        );
        // Best-effort: a poisoned lock or a write error must never unwind the host.
        if let Ok(mut sink) = self.sink.lock() {
            let _ = sink.write_all(line.as_bytes());
            let _ = sink.flush();
        }
    }

    fn flush(&self) {
        if let Ok(mut sink) = self.sink.lock() {
            let _ = sink.flush();
        }
    }
}

/// Install the file logger at `<data_dir>/host.log` and return that path (so the caller can log
/// where it landed). Idempotent — a second call (e.g. another `serve` in one process) just keeps the
/// existing sink — and infallible by contract: any setup failure is reported on stderr and the host
/// runs on without a log rather than refusing to start.
pub fn init(data_dir: &Path, level: LevelFilter) -> PathBuf {
    let path = data_dir.join("host.log");
    if LOGGER.get().is_some() {
        log::set_max_level(level);
        return path;
    }
    let _ = std::fs::create_dir_all(data_dir);
    rotate_if_large(&path, MAX_BYTES);
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => {
            let logger = FileLogger {
                sink: Mutex::new(file),
                level,
            };
            if LOGGER.set(logger).is_ok() {
                // The `OnceLock` value lives for the rest of the process, satisfying `'static`.
                if log::set_logger(LOGGER.get().expect("just set")).is_ok() {
                    log::set_max_level(level);
                }
            }
        }
        Err(e) => {
            eprintln!(
                "mailmate-native-host: file logging disabled (cannot open {}): {e}",
                path.display()
            );
        }
    }
    path
}

/// Resolve the log level from `MAILMATE_LOG` (default `info`).
pub fn level_from_env() -> LevelFilter {
    std::env::var("MAILMATE_LOG")
        .ok()
        .as_deref()
        .map(parse_level)
        .unwrap_or(LevelFilter::Info)
}

/// Parse a level string (case/space-insensitive); anything unrecognized falls back to `info`.
fn parse_level(s: &str) -> LevelFilter {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" => LevelFilter::Off,
        "error" => LevelFilter::Error,
        "warn" => LevelFilter::Warn,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        _ => LevelFilter::Info,
    }
}

/// A compact UTC timestamp, `YYYY-MM-DD HH:MM:SS.mmm` (millisecond precision; no offset — the host
/// runs unattended, so an unambiguous UTC log is friendlier than a local offset).
fn now_ts() -> String {
    let fmt = format_description!(
        "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]"
    );
    OffsetDateTime::now_utc()
        .format(fmt)
        .unwrap_or_else(|_| "0000-00-00 00:00:00.000".to_owned())
}

/// Roll `path` aside to `path.old` when it exceeds `max` bytes; a missing file is a no-op.
fn rotate_if_large(path: &Path, max: u64) {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > max {
            let old = PathBuf::from(format!("{}.old", path.display()));
            let _ = std::fs::rename(path, old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Level;
    use std::io::Read;

    #[test]
    fn parses_every_level_and_defaults_to_info() {
        assert_eq!(parse_level("off"), LevelFilter::Off);
        assert_eq!(parse_level("ERROR"), LevelFilter::Error);
        assert_eq!(parse_level(" warn "), LevelFilter::Warn);
        assert_eq!(parse_level("Info"), LevelFilter::Info);
        assert_eq!(parse_level("debug"), LevelFilter::Debug);
        assert_eq!(parse_level("trace"), LevelFilter::Trace);
        assert_eq!(parse_level("nonsense"), LevelFilter::Info);
        assert_eq!(parse_level(""), LevelFilter::Info);
    }

    #[test]
    fn timestamp_has_the_fixed_compact_shape() {
        let ts = now_ts();
        // `2026-06-20 12:34:56.789` — 23 chars, space at 10, dashes/colons/dot in place.
        assert_eq!(ts.len(), 23, "{ts:?}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], " ");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[19..20], ".");
    }

    #[test]
    fn writes_enabled_records_and_drops_records_below_the_level() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.log");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let logger = FileLogger {
            sink: Mutex::new(file),
            level: LevelFilter::Info,
        };

        logger.log(
            &Record::builder()
                .level(Level::Info)
                .target("mailmate::host")
                .args(format_args!("list_models ok: {} models", 3))
                .build(),
        );
        // Below the configured level → must not be written.
        logger.log(
            &Record::builder()
                .level(Level::Debug)
                .target("mailmate::host")
                .args(format_args!("verbose noise"))
                .build(),
        );
        logger.flush();

        let mut contents = String::new();
        File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert!(contents.contains("list_models ok: 3 models"), "{contents:?}");
        assert!(contents.contains("INFO"), "{contents:?}");
        assert!(contents.contains("mailmate::host"), "{contents:?}");
        assert!(!contents.contains("verbose noise"), "{contents:?}");
    }

    #[test]
    fn rotates_only_when_over_the_size_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.log");

        std::fs::write(&path, vec![b'x'; 64]).unwrap();
        rotate_if_large(&path, 16); // 64 > 16 → roll aside
        assert!(!path.exists(), "the oversized log should have been rolled aside");
        assert!(dir.path().join("host.log.old").exists());

        std::fs::write(&path, b"small").unwrap();
        rotate_if_large(&path, 16); // 5 < 16 → keep
        assert!(path.exists(), "a small log must be left in place");
    }
}
