//! Size-capped rolling log: 5 files × 5 MiB.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_BYTES: u64 = 5 * 1024 * 1024;
const MAX_FILES: u32 = 5;

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
        let _ = writeln!(file, "{line}");
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

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
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
}
