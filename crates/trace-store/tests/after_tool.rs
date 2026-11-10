//! AfterTool rows on a real SQLite trace and blob store: the raw tool response is the primary
//! payload, the raw tool input reported with it is a second blob, `before_event_id` and `outcome`
//! round-trip, and traces written before these columns existed (issue #3 PR 3 and earlier) are
//! read with `None` in their place and are never changed by a reader or by a refused writer.

use std::path::Path;

use rusqlite::{params, Connection};
use trace_store::{
    read_events, read_input_payload, read_payload, BlobHash, Error, Event, EventKind, Outcome,
    TraceWriter, BLOBS_DIR, TRACE_FILE,
};

const INPUT: &str = r#"{"file_path":"/w/src/lib.rs",  "old_string":"a","new_string":"b"}"#;

fn before(id: u64, input: &str) -> Event {
    Event {
        event_id: id,
        slot: "slot-1".into(),
        session_id: "sess-a".into(),
        vclock: id.to_le_bytes().to_vec(),
        kind: EventKind::BeforeTool,
        tool: Some("Edit".into()),
        path: None,
        payload_hash: None,
        payload: Some(input.into()),
        input_payload_hash: None,
        input_payload: None,
        tool_use_id: Some(format!("toolu_{id}")),
        before_event_id: None,
        outcome: None,
        wall_time: 0,
    }
}

fn after(id: u64, before_event_id: Option<u64>, input: &str, response: &str) -> Event {
    Event {
        kind: EventKind::AfterTool,
        payload: Some(response.into()),
        input_payload: Some(input.into()),
        before_event_id,
        outcome: Some(Outcome::Success),
        ..before(id, input)
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

fn hash(bytes: &str) -> Vec<u8> {
    BlobHash::of(bytes.as_bytes()).as_bytes().to_vec()
}

#[test]
fn an_after_tool_row_keeps_its_response_as_payload_and_its_input_as_a_second_blob() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    // Every JSON value is a valid response, explicit null included, kept byte for byte.
    let responses = [
        "null",
        "42",
        r#""plain text""#,
        r#"{"filePath":"/w/src/lib.rs",  "ok":true}"#,
    ];
    w.append(&before(1, INPUT)).unwrap();
    for (i, response) in responses.iter().enumerate() {
        w.append(&after(i as u64 + 2, Some(1), INPUT, response))
            .unwrap();
    }

    let rows = read_events(&tmp.path().join(TRACE_FILE)).unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].before_event_id, None);
    assert_eq!(rows[0].outcome, None);
    assert_eq!(rows[0].input_payload_hash, None);
    for (row, response) in rows[1..].iter().zip(responses) {
        assert_eq!(row.kind, EventKind::AfterTool);
        assert_eq!(row.before_event_id, Some(1));
        assert_eq!(row.outcome, Some(Outcome::Success));
        assert_eq!(row.payload, None, "no inline body in a new row");
        assert_eq!(row.payload_hash, Some(hash(response)));
        assert_eq!(row.input_payload_hash, Some(hash(INPUT)));
        assert_eq!(
            read_payload(tmp.path(), row).unwrap().as_deref(),
            Some(response.as_bytes())
        );
        assert_eq!(
            read_input_payload(tmp.path(), row).unwrap().as_deref(),
            Some(INPUT.as_bytes())
        );
    }
    // The input the BeforeTool and every AfterTool share is stored once.
    let mut want: Vec<String> = std::iter::once(INPUT)
        .chain(responses)
        .map(|b| BlobHash::of(b.as_bytes()).to_string())
        .collect();
    want.sort();
    assert_eq!(blob_names(tmp.path()), want);
    // NULL in the database, not empty strings, for the BeforeTool's AfterTool-only columns.
    let (before_id, outcome, input_hash): (Option<i64>, Option<String>, Option<Vec<u8>>) =
        Connection::open(tmp.path().join(TRACE_FILE))
            .unwrap()
            .query_row(
                "SELECT before_event_id, outcome, input_payload_hash FROM events
                 WHERE event_id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
    assert_eq!((before_id, outcome, input_hash), (None, None, None));
}

#[test]
fn an_orphan_after_tool_keeps_its_input_and_response_with_no_before_event() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    w.append(&after(1, None, INPUT, "null")).unwrap();
    let row = read_events(&tmp.path().join(TRACE_FILE)).unwrap().remove(0);
    assert_eq!(row.before_event_id, None);
    assert_eq!(
        read_input_payload(tmp.path(), &row).unwrap().as_deref(),
        Some(INPUT.as_bytes())
    );
    assert_eq!(
        read_payload(tmp.path(), &row).unwrap().as_deref(),
        Some(&b"null"[..])
    );
}

#[test]
fn a_failed_insert_leaves_both_blobs_as_orphans_and_no_row() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    w.append(&before(1, "{}")).unwrap();
    let err = w.append(&after(1, Some(1), INPUT, "7")).unwrap_err();
    assert!(matches!(err, Error::Sqlite(_)), "{err}");
    let rows = read_events(&tmp.path().join(TRACE_FILE)).unwrap();
    assert_eq!(rows.len(), 1, "only the first row");
    // Both blobs were stored before the INSERT, and stay as orphans no row names.
    let mut want = vec![
        BlobHash::of(b"{}").to_string(),
        BlobHash::of(INPUT.as_bytes()).to_string(),
        BlobHash::of(b"7").to_string(),
    ];
    want.sort();
    assert_eq!(blob_names(tmp.path()), want);
}

#[test]
fn an_unknown_outcome_is_an_error_not_a_guess() {
    let tmp = tempfile::tempdir().unwrap();
    drop(TraceWriter::create(tmp.path()).unwrap());
    Connection::open(tmp.path().join(TRACE_FILE))
        .unwrap()
        .execute(
            "INSERT INTO events (event_id, agent_id, session_id, vclock, kind, wall_time, outcome)
             VALUES (1, 'slot-1', 's', x'', 'AfterTool', 0, 'maybe')",
            [],
        )
        .unwrap();
    let err = read_events(&tmp.path().join(TRACE_FILE)).unwrap_err();
    assert!(
        matches!(err, Error::UnknownOutcome { event_id: 1, ref outcome } if outcome == "maybe"),
        "{err}"
    );
}

/// The table of the issue #3 PR 3 writer (`717ab08`): `tool_use_id`, and none of the AfterTool
/// columns.
const PR_3_SCHEMA: &str = "
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
    wall_time    INTEGER NOT NULL,
    tool_use_id  TEXT
);
CREATE TRIGGER IF NOT EXISTS events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'trace is append-only'); END;
CREATE TRIGGER IF NOT EXISTS events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'trace is append-only'); END;
";

fn columns(run_dir: &Path) -> Vec<String> {
    let conn = Connection::open(run_dir.join(TRACE_FILE)).unwrap();
    let mut stmt = conn
        .prepare("SELECT name FROM pragma_table_info('events')")
        .unwrap();
    let names = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    names
}

const AFTER_COLUMNS: [&str; 3] = ["before_event_id", "outcome", "input_payload_hash"];

#[test]
fn a_pr_3_trace_reads_with_no_after_tool_columns_and_is_not_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("pr3");
    std::fs::create_dir_all(&run_dir).unwrap();
    let conn = Connection::open(run_dir.join(TRACE_FILE)).unwrap();
    conn.execute_batch(PR_3_SCHEMA).unwrap();
    let hash = trace_store::BlobStore::open(&run_dir)
        .unwrap()
        .put(INPUT.as_bytes())
        .unwrap();
    conn.execute(
        "INSERT INTO events
            (event_id, agent_id, session_id, vclock, kind, tool, path, payload_hash,
             tool_input, wall_time, tool_use_id)
         VALUES (1, 'slot-1', 's', x'0100000000000000', 'BeforeTool', 'Edit', NULL, ?1,
                 NULL, 0, 'toolu_1')",
        params![hash.as_bytes().to_vec()],
    )
    .unwrap();
    drop(conn);
    let bytes = std::fs::read(run_dir.join(TRACE_FILE)).unwrap();
    let cols = columns(&run_dir);

    let rows = read_events(&run_dir.join(TRACE_FILE)).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].tool_use_id.as_deref(), Some("toolu_1"));
    assert_eq!(
        (
            rows[0].before_event_id,
            rows[0].outcome,
            &rows[0].input_payload_hash
        ),
        (None, None, &None),
        "nothing is invented"
    );
    assert_eq!(
        read_payload(&run_dir, &rows[0]).unwrap().as_deref(),
        Some(INPUT.as_bytes())
    );
    // Not a resume path: the writer refuses it before touching the schema.
    let err = TraceWriter::create(&run_dir).unwrap_err();
    assert!(matches!(err, Error::RunNotEmpty { events: 1, .. }), "{err}");
    assert_eq!(std::fs::read(run_dir.join(TRACE_FILE)).unwrap(), bytes);
    assert_eq!(columns(&run_dir), cols);
    assert!(AFTER_COLUMNS.iter().all(|c| !cols.iter().any(|n| n == c)));
}

#[test]
fn an_empty_pr_3_trace_gets_the_after_tool_columns_and_then_keeps_them() {
    let tmp = tempfile::tempdir().unwrap();
    Connection::open(tmp.path().join(TRACE_FILE))
        .unwrap()
        .execute_batch(PR_3_SCHEMA)
        .unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    let cols = columns(tmp.path());
    assert!(AFTER_COLUMNS.iter().all(|c| cols.iter().any(|n| n == c)));
    w.append(&after(1, None, INPUT, "null")).unwrap();
    let row = read_events(&tmp.path().join(TRACE_FILE)).unwrap().remove(0);
    assert_eq!(row.outcome, Some(Outcome::Success));
    assert_eq!(row.input_payload_hash, Some(hash(INPUT)));
    // The append-only triggers still hold after the columns were added.
    let conn = Connection::open(tmp.path().join(TRACE_FILE)).unwrap();
    assert!(conn
        .execute("UPDATE events SET outcome = 'success'", [])
        .is_err());
    assert!(conn.execute("DELETE FROM events", []).is_err());
}
