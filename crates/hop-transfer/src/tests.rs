use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{recv_all, send_all, Frame, TransferError, CHUNK_BYTES};
use crate::manifest::Entry;
use crate::verify::hash_bytes;
use crate::write_frame;
use crate::Manifest;

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

async fn roundtrip(roots: &[PathBuf], dest: &std::path::Path) -> Result<Manifest, TransferError> {
    let (mut client, mut server) = tokio::io::duplex(256 * 1024);
    let staging = scratch();
    let cancel = Arc::new(AtomicBool::new(false));
    let send_cancel = cancel.clone();
    let send_roots = roots.to_vec();
    let send =
        tokio::spawn(
            async move { send_all(&mut client, 42, &send_roots, &send_cancel, |_| {}).await },
        );
    let recv = recv_all(&mut server, staging.path(), dest, &cancel, |_| {}).await;
    let sent = send.await.unwrap()?;
    let got = recv?;
    assert_eq!(sent, got);
    Ok(got)
}

#[tokio::test]
async fn one_hundred_small_files_and_an_empty_file() {
    let src = scratch();
    let folder = src.path().join("pack");
    fs::create_dir(&folder).unwrap();
    for i in 0..100 {
        fs::write(folder.join(format!("f{i:03}.txt")), format!("body-{i}")).unwrap();
    }
    fs::write(folder.join("empty.dat"), b"").unwrap();
    let nested = folder.join("sub");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("deep.txt"), b"deep").unwrap();

    let dest_dir = scratch();
    roundtrip(&[folder], dest_dir.path()).await.unwrap();
    let pack = dest_dir.path().join("pack");
    assert_eq!(fs::read(pack.join("f000.txt")).unwrap(), b"body-0");
    assert_eq!(fs::read(pack.join("f099.txt")).unwrap(), b"body-99");
    assert_eq!(fs::read(pack.join("empty.dat")).unwrap(), b"");
    assert_eq!(
        fs::read(pack.join("sub").join("deep.txt")).unwrap(),
        b"deep"
    );
}

#[tokio::test]
async fn nfd_file_name_arrives_as_nfc() {
    let src = scratch();
    let nfd = "cafe\u{0301}.txt";
    let dest_dir = scratch();
    let folder = src.path().join("notes");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join(nfd), b"latte").unwrap();
    roundtrip(&[folder], dest_dir.path()).await.unwrap();
    let nfc = dest_dir.path().join("notes").join("caf\u{e9}.txt");
    assert_eq!(fs::read(&nfc).unwrap(), b"latte");
}

#[tokio::test]
async fn cancel_deletes_partial_files() {
    let src = scratch();
    let big = src.path().join("big.bin");
    {
        let mut f = fs::File::create(&big).unwrap();
        let chunk = vec![7u8; CHUNK_BYTES];
        for _ in 0..4 {
            f.write_all(&chunk).unwrap();
        }
    }
    let (mut client, mut server) = tokio::io::duplex(8 * 1024);
    let staging = scratch();
    let dest_dir = scratch();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let stage_path = staging.path().to_path_buf();
    let dest_path = dest_dir.path().to_path_buf();
    let recv = tokio::spawn(async move {
        recv_all(&mut server, &stage_path, &dest_path, &flag, {
            let flag = cancel.clone();
            move |_| {
                flag.store(true, Ordering::Relaxed);
            }
        })
        .await
    });
    let send_cancel = Arc::new(AtomicBool::new(false));
    let send_result = send_all(&mut client, 7, &[big], &send_cancel, |_| {}).await;
    let recv_result = recv.await.unwrap();
    assert!(recv_result.is_err());
    assert!(matches!(
        recv_result.unwrap_err(),
        TransferError::Cancelled | TransferError::Io(_)
    ));
    let _ = send_result;
    assert!(fs::read_dir(dest_dir.path()).unwrap().next().is_none());
    assert!(fs::read_dir(staging.path()).unwrap().next().is_none());
}

#[tokio::test]
async fn bad_hash_does_not_publish() {
    let (mut client, mut server) = tokio::io::duplex(64 * 1024);
    let manifest = Manifest {
        id: 9,
        entries: vec![Entry {
            rel_path: "a.txt".into(),
            bytes: 4,
            mtime_ms: 0,
            is_dir: false,
        }],
    };
    let writer = tokio::spawn(async move {
        write_frame(&mut client, &Frame::Manifest(manifest))
            .await
            .unwrap();
        write_frame(
            &mut client,
            &Frame::Chunk {
                id: 9,
                file: 0,
                offset: 0,
                data: b"good".to_vec(),
            },
        )
        .await
        .unwrap();
        write_frame(
            &mut client,
            &Frame::FileHash {
                id: 9,
                file: 0,
                blake3: hash_bytes(b"nope"),
            },
        )
        .await
        .unwrap();
    });
    let staging = scratch();
    let dest_dir = scratch();
    let cancel = Arc::new(AtomicBool::new(false));
    let err = recv_all(
        &mut server,
        staging.path(),
        dest_dir.path(),
        &cancel,
        |_| {},
    )
    .await
    .unwrap_err();
    assert!(matches!(err, TransferError::HashMismatch(_)));
    writer.await.unwrap();
    assert!(fs::read_dir(dest_dir.path()).unwrap().next().is_none());
    assert!(fs::read_dir(staging.path()).unwrap().next().is_none());
}

#[tokio::test]
#[ignore = "writes 2 GiB; run with --ignored before a release"]
async fn two_gigabyte_file_round_trips() {
    let src = scratch();
    let path = src.path().join("huge.bin");
    {
        let mut f = fs::File::create(&path).unwrap();
        let chunk = vec![1u8; 1024 * 1024];
        for _ in 0..(2 * 1024) {
            f.write_all(&chunk).unwrap();
        }
    }
    let dest_dir = scratch();
    roundtrip(&[path], dest_dir.path()).await.unwrap();
    let got = dest_dir.path().join("huge.bin");
    assert_eq!(fs::metadata(&got).unwrap().len(), 2 * 1024 * 1024 * 1024);
}
