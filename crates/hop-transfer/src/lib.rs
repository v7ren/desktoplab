//! Folder manifests, chunked streaming, and BLAKE3 verification.
//!
//! Bytes land in a staging directory and move to the destination only after
//! every file's hash matches. Cancel or a bad hash deletes the partials (R14).

#![forbid(unsafe_code)]

mod manifest;
mod recv;
mod send;
mod verify;

pub use manifest::{build_manifest, safe_rel, Entry, Manifest};
pub use recv::{clear_staging, recv_all};
pub use send::send_all;
pub use verify::{hash_bytes, hash_file};

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CHUNK_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub id: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

#[derive(Debug, Error)]
pub enum TransferError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("codec: {0}")]
    Codec(String),
    #[error("frame exceeds {0} bytes")]
    TooLarge(usize),
    #[error("path escapes the transfer root")]
    BadPath,
    #[error("cancelled")]
    Cancelled,
    #[error("blake3 mismatch for {0}")]
    HashMismatch(String),
    #[error("unexpected frame")]
    Protocol,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Frame {
    Manifest(Manifest),
    Chunk {
        id: u64,
        file: u32,
        offset: u64,
        data: Vec<u8>,
    },
    FileHash {
        id: u64,
        file: u32,
        blake3: [u8; 32],
    },
    Commit {
        id: u64,
    },
    Cancel {
        id: u64,
    },
}

pub(crate) async fn write_frame<W>(w: &mut W, frame: &Frame) -> Result<(), TransferError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let body = postcard::to_allocvec(frame).map_err(|e| TransferError::Codec(e.to_string()))?;
    if body.len() > 8 * 1024 * 1024 {
        return Err(TransferError::TooLarge(body.len()));
    }
    let len = u32::try_from(body.len()).map_err(|_| TransferError::TooLarge(body.len()))?;
    w.write_all(&len.to_le_bytes()).await?;
    w.write_all(&body).await?;
    Ok(())
}

pub(crate) async fn read_frame<R>(r: &mut R) -> Result<Option<Frame>, TransferError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 4];
    let mut filled = 0;
    while filled < 4 {
        let n = r.read(&mut len_buf[filled..]).await?;
        if n == 0 {
            if filled == 0 {
                return Ok(None);
            }
            return Err(TransferError::Protocol);
        }
        filled += n;
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 8 * 1024 * 1024 {
        return Err(TransferError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    let frame = postcard::from_bytes(&body).map_err(|e| TransferError::Codec(e.to_string()))?;
    Ok(Some(frame))
}
