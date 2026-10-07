//! BLAKE3 of a file's bytes. The hash is the only thing that authorizes a rename
//! out of the staging directory.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::TransferError;

pub fn hash_file(path: &Path) -> Result<[u8; 32], TransferError> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(*hasher.finalize().as_bytes())
}

pub fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}
