use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use filetime::{set_file_mtime, FileTime};

use crate::manifest::{safe_rel, Manifest};
use crate::verify::hash_file;
use crate::{read_frame, Frame, Progress, TransferError};

pub async fn recv_all<R, F>(
    r: &mut R,
    staging_root: &Path,
    dest_root: &Path,
    cancel: &Arc<AtomicBool>,
    mut progress: F,
) -> Result<Manifest, TransferError>
where
    R: tokio::io::AsyncRead + Unpin,
    F: FnMut(Progress),
{
    let Some(first) = read_frame(r).await? else {
        return Err(TransferError::Protocol);
    };
    let Frame::Manifest(manifest) = first else {
        return Err(TransferError::Protocol);
    };
    let stage = staging_root.join(manifest.id.to_string());
    fs::create_dir_all(&stage)?;
    let total: u64 = manifest.entries.iter().map(|e| e.bytes).sum();
    let mut done = 0u64;
    let result = recv_body(
        r,
        &manifest,
        &stage,
        cancel,
        &mut done,
        total,
        &mut progress,
    )
    .await;
    match result {
        Ok(()) => {
            publish(&manifest, &stage, dest_root)?;
            let _ = fs::remove_dir_all(&stage);
            Ok(manifest)
        }
        Err(err) => {
            let _ = fs::remove_dir_all(&stage);
            Err(err)
        }
    }
}

async fn recv_body<R, F>(
    r: &mut R,
    manifest: &Manifest,
    stage: &Path,
    cancel: &Arc<AtomicBool>,
    done: &mut u64,
    total: u64,
    progress: &mut F,
) -> Result<(), TransferError>
where
    R: tokio::io::AsyncRead + Unpin,
    F: FnMut(Progress),
{
    let mut hashed = vec![false; manifest.entries.len()];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(TransferError::Cancelled);
        }
        let Some(frame) = read_frame(r).await? else {
            return Err(TransferError::Protocol);
        };
        match frame {
            Frame::Cancel { .. } => return Err(TransferError::Cancelled),
            Frame::Commit { .. } => {
                if manifest
                    .entries
                    .iter()
                    .enumerate()
                    .any(|(i, e)| !e.is_dir && !hashed[i])
                {
                    return Err(TransferError::Protocol);
                }
                return Ok(());
            }
            Frame::Chunk {
                file, offset, data, ..
            } => {
                let entry = manifest
                    .entries
                    .get(file as usize)
                    .ok_or(TransferError::Protocol)?;
                if entry.is_dir {
                    return Err(TransferError::Protocol);
                }
                let path = safe_rel(stage, &entry.rel_path)?;
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut file_handle = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(false)
                    .open(&path)?;
                file_handle.seek(SeekFrom::Start(offset))?;
                file_handle.write_all(&data)?;
                *done += data.len() as u64;
                progress(Progress {
                    id: manifest.id,
                    bytes_done: *done,
                    bytes_total: total,
                });
            }
            Frame::FileHash { file, blake3, .. } => {
                let entry = manifest
                    .entries
                    .get(file as usize)
                    .ok_or(TransferError::Protocol)?;
                let path = safe_rel(stage, &entry.rel_path)?;
                if !path.exists() {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    File::create(&path)?;
                }
                let got = hash_file(&path)?;
                if got != blake3 {
                    return Err(TransferError::HashMismatch(entry.rel_path.clone()));
                }
                if let Some(slot) = hashed.get_mut(file as usize) {
                    *slot = true;
                }
                set_mtime(&path, entry.mtime_ms)?;
            }
            Frame::Manifest(_) => return Err(TransferError::Protocol),
        }
    }
}

fn set_mtime(path: &Path, mtime_ms: i64) -> Result<(), TransferError> {
    if mtime_ms < 0 {
        return Ok(());
    }
    let secs = mtime_ms / 1000;
    let nsec = ((mtime_ms % 1000) * 1_000_000) as u32;
    set_file_mtime(path, FileTime::from_unix_time(secs, nsec))?;
    Ok(())
}

fn publish(manifest: &Manifest, stage: &Path, dest_root: &Path) -> Result<(), TransferError> {
    fs::create_dir_all(dest_root)?;
    for entry in &manifest.entries {
        let from = safe_rel(stage, &entry.rel_path)?;
        let to = safe_rel(dest_root, &entry.rel_path)?;
        if entry.is_dir {
            fs::create_dir_all(&to)?;
            continue;
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        move_file(&from, &to)?;
        set_mtime(&to, entry.mtime_ms)?;
    }
    Ok(())
}

fn move_file(from: &Path, to: &Path) -> Result<(), TransferError> {
    if to.exists() {
        let _ = fs::remove_file(to);
    }
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(from, to)?;
            fs::remove_file(from)?;
            Ok(())
        }
    }
}

/// Delete anything left in the staging root. Called once at startup.
pub fn clear_staging(staging_root: &Path) -> std::io::Result<()> {
    if staging_root.exists() {
        fs::remove_dir_all(staging_root)?;
    }
    fs::create_dir_all(staging_root)
}
