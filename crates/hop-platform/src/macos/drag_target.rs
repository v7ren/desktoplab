//! Native drag of promised files. Refusal falls back to `~/Downloads/DevHop` (R16).

use std::path::{Path, PathBuf};

pub fn fallback_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Downloads").join("DevHop")
}

pub fn begin(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    // The files are already on disk (staged by hop-transfer). Publishing them
    // on the drag pasteboard lets Finder and Mail take the drop. A target that
    // refuses promises is handled by `ensure_fallback`.
    crate::macos::clipboard_files::offer_files(paths)?;
    Ok(())
}

pub fn ensure_fallback(paths: &[PathBuf]) -> Result<PathBuf, String> {
    let dir = fallback_dir();
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    for path in paths {
        let name = path
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("file"));
        let dest = unique(&dir, name);
        if path != dest {
            let _ = std::fs::copy(path, dest);
        }
    }
    Ok(dir)
}

fn unique(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    dir.join(format!("copy-{}", name.to_string_lossy()))
}
