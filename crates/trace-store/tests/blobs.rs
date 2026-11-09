//! The blob store on a real file system: content addressing, refusals, and concurrent writers.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use trace_store::{BlobError, BlobHash, BlobStore, BLOBS_DIR};

/// Every file name in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn bytes_round_trip_exactly_and_survive_reopening_the_store() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("runs").join("r1");
    fs::create_dir_all(&run_dir).unwrap();
    let store = BlobStore::open(&run_dir).unwrap();
    assert_eq!(store.dir(), run_dir.join(BLOBS_DIR));

    // JSON escapes, multibyte UTF-8 and arbitrary binary must come back unchanged.
    let input = r#"{"file_path":"src/lib.rs","old_string":"a\r\n","new_string":"é"}"#.as_bytes();
    let binary: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
    let mut hashes = Vec::new();
    for bytes in [input, &binary[..], &b""[..]] {
        let hash = store.put(bytes).unwrap();
        assert_eq!(store.get(&hash).unwrap(), bytes);
        assert_eq!(fs::read(store.path(&hash)).unwrap(), bytes);
        hashes.push(hash);
    }

    drop(store);
    let reopened = BlobStore::open(&run_dir).unwrap();
    assert_eq!(reopened.get(&hashes[0]).unwrap(), input);
    assert_eq!(reopened.get(&hashes[1]).unwrap(), binary);
}

#[test]
fn opening_a_missing_run_directory_is_refused_and_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("runs").join("r1");

    let err = BlobStore::open(&run_dir).unwrap_err();
    match &err {
        BlobError::Io { path, source } => {
            assert_eq!(path, &run_dir);
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("expected Io, got {other}"),
    }
    assert!(
        names(tmp.path()).is_empty(),
        "open created {:?}",
        names(tmp.path())
    );
}

#[test]
fn hashes_and_names_match_the_official_blake3_vectors() {
    // From BLAKE3's test_vectors.json, "hash" field. The input of length n is the bytes
    // 0, 1, ..., 250, 0, 1, ... (each byte is its index mod 251).
    let vectors = [
        (
            0,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        ),
        (
            1,
            "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213",
        ),
        (
            1024,
            "42214739f095a406f3fc83deb889744ac00df831c10daa55189b5d121c855af7",
        ),
    ];
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::open(tmp.path()).unwrap();
    for (len, hex) in vectors {
        let input: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let hash = store.put(&input).unwrap();
        assert_eq!(hash.to_string(), hex, "input length {len}");
        assert_eq!(hash, BlobHash::of(&input));
        assert!(store.dir().join(hex).is_file(), "input length {len}");
    }
}

#[test]
fn equal_content_is_stored_once_and_not_rewritten() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::open(tmp.path()).unwrap();
    let first = store.put(b"same bytes").unwrap();
    let inode = fs::metadata(store.path(&first)).unwrap().ino();

    let second = store.put(b"same bytes").unwrap();
    let from_other_store = BlobStore::open(tmp.path())
        .unwrap()
        .put(b"same bytes")
        .unwrap();

    assert_eq!(first, second);
    assert_eq!(first, from_other_store);
    assert_eq!(names(store.dir()), vec![first.to_string()]);
    // Same inode: the intact blob was left alone, not replaced or truncated.
    assert_eq!(fs::metadata(store.path(&first)).unwrap().ino(), inode);
}

#[test]
fn different_content_gets_distinct_hashes_and_files() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::open(tmp.path()).unwrap();
    let a = store.put(b"content a").unwrap();
    let b = store.put(b"content b").unwrap();
    let a_newline = store.put(b"content a\n").unwrap();

    assert_ne!(a, b);
    assert_ne!(a, a_newline);
    let mut expected = vec![a.to_string(), b.to_string(), a_newline.to_string()];
    expected.sort();
    assert_eq!(names(store.dir()), expected);
    assert_eq!(store.get(&a).unwrap(), b"content a");
    assert_eq!(store.get(&b).unwrap(), b"content b");
    assert_eq!(store.get(&a_newline).unwrap(), b"content a\n");
}

#[test]
fn a_blob_that_was_never_stored_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::open(tmp.path()).unwrap();
    store.put(b"something else").unwrap();

    let absent = BlobHash::of(b"never stored");
    let err = store.get(&absent).unwrap_err();
    assert!(
        matches!(err, BlobError::Missing { hash } if hash == absent),
        "{err}"
    );
}

#[test]
fn corrupt_content_is_refused_and_a_put_of_the_real_content_replaces_it() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::open(tmp.path()).unwrap();
    let original = b"the real tool input".to_vec();
    let hash = store.put(&original).unwrap();
    let path = store.path(&hash);

    for damaged in [&b"the real tool"[..], b"the real tool inpuT", b""] {
        fs::write(&path, damaged).unwrap();
        let err = store.get(&hash).unwrap_err();
        match err {
            BlobError::Corrupt {
                path: p,
                expected,
                actual,
            } => {
                assert_eq!(p, path);
                assert_eq!(expected, hash);
                assert_eq!(actual, BlobHash::of(damaged));
            }
            other => panic!("expected Corrupt, got {other}"),
        }
    }

    assert_eq!(store.put(&original).unwrap(), hash);
    assert_eq!(store.get(&hash).unwrap(), original);
    assert_eq!(names(store.dir()), vec![hash.to_string()]);
}

#[test]
fn concurrent_identical_puts_leave_one_complete_blob_and_no_temporary_files() {
    const WRITERS: usize = 16;
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().to_path_buf();
    // Large enough that a write takes several syscalls, so a published prefix would be seen.
    let content: Arc<Vec<u8>> = Arc::new((0..4_000_000u32).map(|i| (i * 7) as u8).collect());
    let expected = BlobHash::of(&content);
    // Open every store first, so nothing that can fail runs before the barrier.
    let stores: Vec<BlobStore> = (0..WRITERS)
        .map(|_| BlobStore::open(&run_dir).unwrap())
        .collect();
    let reader_store = BlobStore::open(&run_dir).unwrap();
    let barrier = Arc::new(Barrier::new(WRITERS + 1));

    let writers: Vec<_> = stores
        .into_iter()
        .map(|store| {
            let (content, barrier) = (content.clone(), barrier.clone());
            thread::spawn(move || {
                barrier.wait();
                store.put(&content).map_err(|e| e.to_string())
            })
        })
        .collect();

    // A reader racing the writers sees either no blob or the whole blob, never part of one. It
    // stops once it sees the blob or every writer has finished, and gives up after a deadline.
    barrier.wait();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let all_finished = writers.iter().all(|w| w.is_finished());
        match reader_store.get(&expected) {
            Ok(bytes) => {
                assert_eq!(bytes.len(), content.len());
                break;
            }
            Err(BlobError::Missing { .. }) if all_finished => break,
            Err(BlobError::Missing { .. }) => {
                assert!(Instant::now() < deadline, "no writer published within 60s");
                thread::yield_now();
            }
            Err(other) => panic!("reader saw a partial or broken blob: {other}"),
        }
    }

    for (i, writer) in writers.into_iter().enumerate() {
        let result = writer.join().expect("writer thread panicked");
        assert_eq!(result, Ok(expected), "writer {i}");
    }
    assert_eq!(names(reader_store.dir()), vec![expected.to_string()]);
    assert_eq!(reader_store.get(&expected).unwrap(), *content);
}

#[test]
fn a_failed_put_removes_its_temporary_file_and_reports_the_path() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BlobStore::open(tmp.path()).unwrap();
    let hash = BlobHash::of(b"cannot be published");
    // A non-empty directory under the blob's name makes the publishing rename fail.
    let blocker = store.path(&hash);
    fs::create_dir(&blocker).unwrap();
    fs::write(blocker.join("keep"), b"x").unwrap();

    let err = store.put(b"cannot be published").unwrap_err();
    match &err {
        BlobError::Io { path, .. } => assert_eq!(path, &blocker),
        other => panic!("expected Io, got {other}"),
    }
    assert_eq!(names(store.dir()), vec![hash.to_string()]);
    assert_eq!(names(&blocker), vec!["keep".to_string()]);
}
