//! A file tree, with names canonicalized to NFC so macOS NFD names match Windows.

use std::fs;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::TransferError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// NFC relative path using `/` separators.
    pub rel_path: String,
    pub bytes: u64,
    pub mtime_ms: i64,
    pub is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: u64,
    pub entries: Vec<Entry>,
}

/// Walk `roots`. Each root contributes its own file name as the first path
/// component, so two selected folders do not collide.
pub fn build_manifest(id: u64, roots: &[std::path::PathBuf]) -> Result<Manifest, TransferError> {
    let mut entries = Vec::new();
    for root in roots {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let name = nfc(&name);
        walk(root, &name, &mut entries)?;
    }
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(Manifest { id, entries })
}

fn walk(path: &Path, rel: &str, out: &mut Vec<Entry>) -> Result<(), TransferError> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    if meta.is_dir() {
        out.push(Entry {
            rel_path: rel.to_string(),
            bytes: 0,
            mtime_ms,
            is_dir: true,
        });
        let mut children: Vec<_> = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(|e| e.file_name());
        for child in children {
            let child_name = nfc(&child.file_name().to_string_lossy());
            let child_rel = format!("{rel}/{child_name}");
            walk(&child.path(), &child_rel, out)?;
        }
    } else if meta.is_file() {
        out.push(Entry {
            rel_path: rel.to_string(),
            bytes: meta.len(),
            mtime_ms,
            is_dir: false,
        });
    }
    Ok(())
}

pub fn nfc(text: &str) -> String {
    text.nfc().collect()
}

/// Join a relative transfer path onto `root`, refusing `..` and absolute parts.
pub fn safe_rel(root: &Path, rel: &str) -> Result<std::path::PathBuf, TransferError> {
    if rel.is_empty() || rel.starts_with('/') || rel.starts_with('\\') {
        return Err(TransferError::BadPath);
    }
    let mut out = root.to_path_buf();
    for part in Path::new(rel).components() {
        match part {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => {
                return Err(TransferError::BadPath);
            }
        }
    }
    if !out.starts_with(root) {
        return Err(TransferError::BadPath);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_segments() {
        let root = Path::new("/tmp/dest");
        assert!(safe_rel(root, "../etc/passwd").is_err());
        assert!(safe_rel(root, "a/../../b").is_err());
        assert!(safe_rel(root, "/abs").is_err());
        let ok = safe_rel(root, "a/b.txt").unwrap();
        assert!(ok.ends_with("b.txt"));
    }

    #[test]
    fn nfd_name_becomes_nfc() {
        let nfd = "cafe\u{0301}.txt";
        let nfc_name = nfc(nfd);
        assert_ne!(nfd, nfc_name);
        assert_eq!(nfc_name, "caf\u{e9}.txt");
    }
}
