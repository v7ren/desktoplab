use std::fs::File;
use std::io::Read;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::manifest::{build_manifest, nfc, Manifest};
use crate::verify::hash_file;
use crate::{write_frame, Frame, Progress, TransferError, CHUNK_BYTES};

pub async fn send_all<W, F>(
    w: &mut W,
    id: u64,
    roots: &[std::path::PathBuf],
    cancel: &Arc<AtomicBool>,
    mut progress: F,
) -> Result<Manifest, TransferError>
where
    W: tokio::io::AsyncWrite + Unpin,
    F: FnMut(Progress),
{
    let manifest = build_manifest(id, roots)?;
    let total: u64 = manifest.entries.iter().map(|e| e.bytes).sum();
    write_frame(w, &Frame::Manifest(manifest.clone())).await?;
    let mut done = 0u64;
    progress(Progress {
        id,
        bytes_done: 0,
        bytes_total: total,
    });
    for (index, entry) in manifest.entries.iter().enumerate() {
        if entry.is_dir {
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            write_frame(w, &Frame::Cancel { id }).await?;
            return Err(TransferError::Cancelled);
        }
        let path = find_source(roots, &entry.rel_path)?;
        let hash = hash_file(&path)?;
        let mut file = File::open(&path)?;
        let mut offset = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                write_frame(w, &Frame::Cancel { id }).await?;
                return Err(TransferError::Cancelled);
            }
            let mut buf = vec![0u8; CHUNK_BYTES];
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            buf.truncate(n);
            write_frame(
                w,
                &Frame::Chunk {
                    id,
                    file: index as u32,
                    offset,
                    data: buf,
                },
            )
            .await?;
            offset += n as u64;
            done += n as u64;
            progress(Progress {
                id,
                bytes_done: done,
                bytes_total: total,
            });
        }
        write_frame(
            w,
            &Frame::FileHash {
                id,
                file: index as u32,
                blake3: hash,
            },
        )
        .await?;
    }
    write_frame(w, &Frame::Commit { id }).await?;
    Ok(manifest)
}

fn find_source(
    roots: &[std::path::PathBuf],
    rel: &str,
) -> Result<std::path::PathBuf, TransferError> {
    for root in roots {
        let name = root
            .file_name()
            .map(|n| nfc(&n.to_string_lossy()))
            .unwrap_or_default();
        if rel == name {
            return Ok(root.clone());
        }
        let prefix = format!("{name}/");
        if let Some(rest) = rel.strip_prefix(&prefix) {
            let mut path = root.clone();
            for part in Path::new(rest).components() {
                if let Component::Normal(segment) = part {
                    let want = nfc(&segment.to_string_lossy());
                    let found = match_child(&path, &want)?;
                    path.push(found);
                }
            }
            return Ok(path);
        }
    }
    Err(TransferError::BadPath)
}

fn match_child(dir: &Path, nfc_name: &str) -> Result<std::ffi::OsString, TransferError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let got = nfc(&entry.file_name().to_string_lossy());
        if got == nfc_name {
            return Ok(entry.file_name());
        }
    }
    Err(TransferError::BadPath)
}
