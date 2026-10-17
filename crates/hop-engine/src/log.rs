//! Size-capped rolling log: 5 files × 5 MiB.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MAX_BYTES: u64 = 5 * 1024 * 1024;
const MAX_FILES: u32 = 5;

/// Set once, so two engines in one process cannot fight over the subscriber.
static TRACING_INIT: OnceLock<()> = OnceLock::new();

pub struct Log {
    path: PathBuf,
    file: Mutex<File>,
}

impl Log {
    pub fn open(dir: &Path) -> io::Result<Self> {
        let logs = dir.join("logs");
        fs::create_dir_all(&logs)?;
        let path = logs.join("devhop.log");
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn write_line(&self, line: &str) {
        let mut file = match self.file.lock() {
            Ok(file) => file,
            Err(_) => return,
        };
        let _ = writeln!(file, "{} {line}", utc_stamp());
        let _ = file.flush();
        if let Ok(meta) = file.metadata() {
            if meta.len() >= MAX_BYTES {
                drop(file);
                let _ = self.rotate();
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn tail(&self, max_bytes: usize) -> String {
        let Ok(bytes) = fs::read(&self.path) else {
            return String::new();
        };
        let start = bytes.len().saturating_sub(max_bytes);
        String::from_utf8_lossy(&bytes[start..]).into_owned()
    }

    fn rotate(&self) -> io::Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new(".")).to_path_buf();
        {
            let _guard = self.file.lock().map_err(|_| io::Error::other("log lock"))?;
        }
        let oldest = dir.join(format!("devhop.log.{MAX_FILES}"));
        if oldest.exists() {
            let _ = fs::remove_file(&oldest);
        }
        for index in (1..MAX_FILES).rev() {
            let from = dir.join(format!("devhop.log.{index}"));
            let to = dir.join(format!("devhop.log.{}", index + 1));
            if from.exists() {
                let _ = fs::rename(&from, &to);
            }
        }
        let _ = fs::rename(&self.path, dir.join("devhop.log.1"));
        let created = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let mut guard = self.file.lock().map_err(|_| io::Error::other("log lock"))?;
        *guard = created;
        Ok(())
    }
}

/// Install the global `tracing` subscriber once, writing structured lines to a
/// rolling file under `<dir>/logs` (daily rotation, the newest [`MAX_FILES`]
/// kept). Every thread logs straight into this file; the engine's user-visible
/// status line stays on [`Log`] and is written separately.
///
/// Initialised at most once per process. Later calls are ignored, so a second
/// engine started in the same process keeps the first subscriber.
pub fn init(dir: &Path) {
    if TRACING_INIT.get().is_some() {
        return;
    }
    let logs = dir.join("logs");
    let _ = fs::create_dir_all(&logs);
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("devhop")
        .filename_suffix("log")
        .max_log_files(MAX_FILES as usize)
        .build(&logs);
    let Ok(appender) = appender else {
        // The log directory could not be opened; keep logging to stderr.
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
        let _ = TRACING_INIT.set(());
        return;
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(appender)
        .try_init();
    let _ = TRACING_INIT.set(());
}

/// A connection's identity for structured log lines: the QUIC connection
/// `stable_id` and the peer's device id. Both are recorded as tracing fields on
/// every line, so a replaced link can be told from the live one.
#[derive(Clone, Copy, Debug)]
pub struct Conn<'a> {
    pub stable_id: usize,
    pub peer: &'a str,
}

impl<'a> Conn<'a> {
    pub fn new(stable_id: usize, peer: &'a str) -> Self {
        Self { stable_id, peer }
    }

    pub fn info(&self, line: impl std::fmt::Display) {
        tracing::info!(stable_id = self.stable_id, peer = self.peer, "{}", line);
    }

    pub fn debug(&self, line: impl std::fmt::Display) {
        tracing::debug!(stable_id = self.stable_id, peer = self.peer, "{}", line);
    }

    pub fn warn(&self, line: impl std::fmt::Display) {
        tracing::warn!(stable_id = self.stable_id, peer = self.peer, "{}", line);
    }
}

/// A span carrying the connection fields for the duration of a task/scope. Use
/// [`Conn`] for one-off events.
pub fn conn_span(stable_id: usize, peer: &str) -> tracing::Span {
    tracing::info_span!("conn", stable_id, peer)
}

/// `YYYY-MM-DD hh:mm:ss.mmmZ` (UTC) without pulling in a date crate.
fn utc_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{s:02}.{:03}Z",
        now.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip_through_the_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let log = Log::open(dir.path()).unwrap();
        log.write_line("hello");
        log.write_line("world");
        let tail = log.tail(1024);
        assert!(tail.contains("hello"));
        assert!(tail.contains("world"));
    }

    #[test]
    fn tracing_lines_carry_the_connection_fields() {
        let dir = tempfile::tempdir().unwrap();
        let writer = tracing_appender::rolling::never(dir.path(), "devhop.log");
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(writer)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            Conn::new(42, "peer-x").info("dialing 10.0.0.1");
            Conn::new(42, "peer-x").warn("lost the connection");
        });
        let text = std::fs::read_to_string(dir.path().join("devhop.log")).unwrap();
        assert!(text.contains("dialing 10.0.0.1"), "log: {text}");
        assert!(text.contains("lost the connection"), "log: {text}");
        assert!(text.contains("stable_id"), "log: {text}");
        assert!(text.contains("42"), "log: {text}");
        assert!(text.contains("peer-x"), "log: {text}");
    }
}
