//! Payloads on a real file system and a real SQLite trace: the writer stores each event's raw
//! tool input as a blob before its row, the row keeps only the hash, and `read_payload` gets the
//! exact bytes back from the blob, or from the inline column of a trace written before blobs.

use std::path::Path;

use rusqlite::{params, Connection};
use trace_store::{
    read_events, read_payload, BlobHash, Error, Event, EventKind, TraceWriter, BLOBS_DIR,
    TRACE_FILE,
};

fn event(id: u64, payload: Option<&str>) -> Event {
    Event {
        event_id: id,
        slot: "slot-1".into(),
        session_id: "sess-a".into(),
        vclock: id.to_le_bytes().to_vec(),
        kind: EventKind::BeforeTool,
        tool: Some("Edit".into()),
        path: None,
        payload_hash: None,
        payload: payload.map(str::to_owned),
        input_payload_hash: None,
        input_payload: None,
        tool_use_id: Some(format!("toolu_{id}")),
        before_event_id: None,
        outcome: None,
        wall_time: 1_700_000_000_000,
    }
}

fn blob_names(run_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(run_dir.join(BLOBS_DIR))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Escapes, odd spacing, non-ASCII and key order must all survive: the payload is bytes, not JSON.
const RAW: &str = "{\"file_path\":\"/w/src/lib.rs\",  \"old_string\":\"a\\n\\\"b\\\"\",\"new_string\":\"\u{e9}\u{1f600}\"}";

#[test]
fn a_new_row_names_its_blob_by_hash_and_the_blob_holds_the_exact_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("runs").join("r1");
    let mut w = TraceWriter::create(&run_dir).unwrap();
    w.append(&event(1, Some(RAW))).unwrap();

    let rows = read_events(&run_dir.join(TRACE_FILE)).unwrap();
    assert_eq!(rows.len(), 1);
    let hash = BlobHash::of(RAW.as_bytes());
    assert_eq!(rows[0].payload_hash.as_deref(), Some(&hash.as_bytes()[..]));
    assert_eq!(rows[0].payload, None, "no inline body in a new row");
    // The NULL is in the database itself, not just in the decoded struct.
    let inline: Option<String> = Connection::open(run_dir.join(TRACE_FILE))
        .unwrap()
        .query_row("SELECT tool_input FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(inline, None);

    assert_eq!(blob_names(&run_dir), vec![hash.to_string()]);
    assert_eq!(
        std::fs::read(run_dir.join(BLOBS_DIR).join(hash.to_string())).unwrap(),
        RAW.as_bytes()
    );
    assert_eq!(
        read_payload(&run_dir, &rows[0]).unwrap().as_deref(),
        Some(RAW.as_bytes())
    );
}

#[test]
fn repeated_payloads_share_one_blob_and_distinct_ones_do_not() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    let other = r#"{"command":"cargo test"}"#;
    w.append(&event(1, Some(RAW))).unwrap();
    w.append(&event(2, Some(other))).unwrap();
    w.append(&event(3, Some(RAW))).unwrap();
    w.append(&event(4, None)).unwrap();

    let rows = read_events(&tmp.path().join(TRACE_FILE)).unwrap();
    assert_eq!(rows[0].payload_hash, rows[2].payload_hash);
    assert_ne!(rows[0].payload_hash, rows[1].payload_hash);
    assert_eq!(rows[3].payload_hash, None, "no payload, no hash, no blob");
    let mut want = vec![
        BlobHash::of(RAW.as_bytes()).to_string(),
        BlobHash::of(other.as_bytes()).to_string(),
    ];
    want.sort();
    assert_eq!(blob_names(tmp.path()), want);
    let payloads: Vec<Option<Vec<u8>>> = rows
        .iter()
        .map(|e| read_payload(tmp.path(), e).unwrap())
        .collect();
    assert_eq!(
        payloads,
        vec![
            Some(RAW.as_bytes().to_vec()),
            Some(other.as_bytes().to_vec()),
            Some(RAW.as_bytes().to_vec()),
            None
        ]
    );
}

/// The schema and INSERT of the issue #2 writer (`6169b5f`, `crates/trace-store/src/lib.rs`):
/// `payload_hash` NULL, the raw input inline in `tool_input`, and no `blobs/` directory.
const ISSUE_2_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    event_id     INTEGER PRIMARY KEY,
    agent_id     TEXT NOT NULL,
    session_id   TEXT NOT NULL,
    vclock       BLOB NOT NULL,
    kind         TEXT NOT NULL,
    tool         TEXT,
    path         TEXT,
    payload_hash BLOB,
    tool_input   TEXT,
    wall_time    INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'trace is append-only'); END;
CREATE TRIGGER IF NOT EXISTS events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'trace is append-only'); END;
";

fn write_issue_2_trace(run_dir: &Path, inputs: &[&str]) {
    std::fs::create_dir_all(run_dir).unwrap();
    let conn = Connection::open(run_dir.join(TRACE_FILE)).unwrap();
    conn.execute_batch(ISSUE_2_SCHEMA).unwrap();
    for (i, input) in inputs.iter().enumerate() {
        conn.execute(
            "INSERT INTO events
                (event_id, agent_id, session_id, vclock, kind, tool, path, payload_hash,
                 tool_input, wall_time)
             VALUES (?1, 'slot-1', 's', x'0100000000000000', 'BeforeTool', 'Edit', NULL, NULL,
                     ?2, 0)",
            params![i as i64 + 1, input],
        )
        .unwrap();
    }
}

#[test]
fn an_issue_2_trace_reads_its_inline_input_and_reading_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("old");
    write_issue_2_trace(&run_dir, &[RAW, r#"{"command":"ls"}"#]);

    let rows = read_events(&run_dir.join(TRACE_FILE)).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|e| e.payload_hash.is_none()));
    assert_eq!(
        read_payload(&run_dir, &rows[0]).unwrap().as_deref(),
        Some(RAW.as_bytes())
    );
    assert_eq!(
        read_payload(&run_dir, &rows[1]).unwrap().as_deref(),
        Some(&br#"{"command":"ls"}"#[..])
    );
    assert!(
        !run_dir.join(BLOBS_DIR).exists(),
        "a reader must not create blobs/"
    );

    // Not a resume path: a writer still refuses the populated legacy run and leaves it alone.
    let err = TraceWriter::create(&run_dir).unwrap_err();
    assert!(matches!(err, Error::RunNotEmpty { events: 2, .. }), "{err}");
    assert!(!run_dir.join(BLOBS_DIR).exists());
    assert_eq!(read_events(&run_dir.join(TRACE_FILE)).unwrap(), rows);
}

#[test]
fn a_missing_or_corrupt_blob_is_an_error_never_wrong_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    w.append(&event(1, Some(RAW))).unwrap();
    let row = read_events(&tmp.path().join(TRACE_FILE)).unwrap().remove(0);
    let blob = tmp
        .path()
        .join(BLOBS_DIR)
        .join(BlobHash::of(RAW.as_bytes()).to_string());

    std::fs::write(&blob, b"tampered").unwrap();
    let err = read_payload(tmp.path(), &row).unwrap_err();
    assert!(
        matches!(err, Error::Blob(trace_store::BlobError::Corrupt { .. })),
        "{err}"
    );
    std::fs::remove_file(&blob).unwrap();
    let err = read_payload(tmp.path(), &row).unwrap_err();
    assert!(
        matches!(err, Error::Blob(trace_store::BlobError::Missing { .. })),
        "{err}"
    );
}

#[test]
fn malformed_payload_columns_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut short = event(1, None);
    short.payload_hash = Some(vec![1; 31]);
    assert!(matches!(
        read_payload(tmp.path(), &short).unwrap_err(),
        Error::BadPayloadHash {
            event_id: 1,
            len: 31
        }
    ));
    let mut both = event(2, Some(RAW));
    both.payload_hash = Some(BlobHash::of(RAW.as_bytes()).as_bytes().to_vec());
    assert!(matches!(
        read_payload(tmp.path(), &both).unwrap_err(),
        Error::TwoPayloads { event_id: 2 }
    ));
}
