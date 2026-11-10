//! Concurrent starts on a real file system: writers racing to create the same new, nested run
//! directory. These tests check outcomes visible on the file system, not crash durability; they
//! do not simulate power loss.

use std::sync::{Arc, Barrier};
use std::thread;

use trace_store::{read_events, Error, TraceWriter, BLOBS_DIR, TRACE_FILE};

#[test]
fn concurrent_starts_on_a_new_nested_run_admit_exactly_one_writer() {
    let tmp = tempfile::tempdir().unwrap();
    // `base` exists; `new-runs/r1` does not, so the racers create it together.
    let base = tmp.path().join("base");
    std::fs::create_dir(&base).unwrap();
    let run_dir = base.join("new-runs").join("r1");
    let n = 8;
    let barrier = Arc::new(Barrier::new(n));
    let handles: Vec<_> = (0..n)
        .map(|_| {
            let (run_dir, barrier) = (run_dir.clone(), barrier.clone());
            thread::spawn(move || {
                barrier.wait();
                TraceWriter::create(&run_dir)
            })
        })
        .collect();
    // Keep every result alive until all are in, so the winner holds the run throughout.
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(winners, 1, "exactly one writer holds the run");
    for r in &results {
        if let Err(e) = r {
            assert!(matches!(e, Error::RunInUse { .. }), "unexpected error: {e}");
        }
    }
    assert!(run_dir.join(BLOBS_DIR).is_dir());
    assert!(read_events(&run_dir.join(TRACE_FILE)).unwrap().is_empty());
}
