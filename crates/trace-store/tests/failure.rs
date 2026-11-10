//! Failed AfterTool rows on a real SQLite trace and blob store: the payload is the raw hook
//! `error` text (no envelope), the reported input is a second blob, and the outcome keeps the
//! interruption flag in its own column, NULL when the report omitted it. Traces written before
//! the `is_interrupt` column (issue #3 PR 4) read unchanged, and stored pairs the writer never
//! writes are errors, never guesses.

use std::path::Path;

use rusqlite::{params, Connection};
use trace_store::{
    read_events, read_input_payload, read_payload, BlobHash, Error, Event, EventKind, Outcome,
    TraceWriter, TRACE_FILE,
};

const INPUT: &str = r#"{"command":"cargo test",  "description":"Run tests"}"#;

fn failure(id: u64, error: &str, is_interrupt: Option<bool>) -> Event {
    Event {
        event_id: id,
        slot: "slot-1".into(),
        session_id: "sess-a".into(),
        vclock: id.to_le_bytes().to_vec(),
        kind: EventKind::AfterTool,
        tool: Some("Bash".into()),
        path: None,
        payload_hash: None,
        payload: Some(error.into()),
        input_payload_hash: None,
        input_payload: Some(INPUT.into()),
        tool_use_id: Some(format!("toolu_{id}")),
        before_event_id: None,
        outcome: Some(Outcome::Failure { is_interrupt }),
        wall_time: 0,
    }
}

fn hash(bytes: &str) -> Vec<u8> {
    BlobHash::of(bytes.as_bytes()).as_bytes().to_vec()
}

fn stored(run_dir: &Path) -> Vec<(Option<String>, Option<i64>)> {
    let conn = Connection::open(run_dir.join(TRACE_FILE)).unwrap();
    let mut stmt = conn
        .prepare("SELECT outcome, is_interrupt FROM events ORDER BY event_id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    rows
}

#[test]
fn failures_round_trip_with_the_raw_error_and_every_interrupt_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    let errors = [
        r#""Exit code 1\n\u00e9""#,
        r#""""#,
        r#""interrupted by user""#,
    ];
    let flags = [Some(false), None, Some(true)];
    for (i, (error, flag)) in errors.iter().zip(flags).enumerate() {
        w.append(&failure(i as u64 + 1, error, flag)).unwrap();
    }
    let rows = read_events(&tmp.path().join(TRACE_FILE)).unwrap();
    assert_eq!(rows.len(), 3);
    for (row, (error, flag)) in rows.iter().zip(errors.iter().zip(flags)) {
        assert_eq!(row.outcome, Some(Outcome::Failure { is_interrupt: flag }));
        assert_eq!(row.payload_hash, Some(hash(error)));
        assert_eq!(row.input_payload_hash, Some(hash(INPUT)));
        assert_eq!(
            read_payload(tmp.path(), row).unwrap().as_deref(),
            Some(error.as_bytes()),
            "the raw error text, escapes included"
        );
        assert_eq!(
            read_input_payload(tmp.path(), row).unwrap().as_deref(),
            Some(INPUT.as_bytes())
        );
    }
    assert_eq!(
        stored(tmp.path()),
        vec![
            (Some("failure".into()), Some(0)),
            (Some("failure".into()), None),
            (Some("failure".into()), Some(1)),
        ],
        "an omitted flag is NULL, never 0"
    );
}

#[test]
fn a_success_stores_no_interrupt_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    let mut ok = failure(1, "null", None);
    ok.outcome = Some(Outcome::Success);
    w.append(&ok).unwrap();
    assert_eq!(stored(tmp.path()), vec![(Some("success".into()), None)]);
    let row = read_events(&tmp.path().join(TRACE_FILE)).unwrap().remove(0);
    assert_eq!(row.outcome, Some(Outcome::Success));
}

#[test]
fn stored_pairs_the_writer_never_writes_are_errors() {
    for (outcome, is_interrupt) in [
        (Some("success"), Some(0)),
        (Some("success"), Some(1)),
        (Some("failure"), Some(2)),
        (Some("failure"), Some(-1)),
        (None, Some(0)),
        (Some("interrupted"), None),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        drop(TraceWriter::create(tmp.path()).unwrap());
        Connection::open(tmp.path().join(TRACE_FILE))
            .unwrap()
            .execute(
                "INSERT INTO events
                    (event_id, agent_id, session_id, vclock, kind, wall_time, outcome, is_interrupt)
                 VALUES (1, 'slot-1', 's', x'', 'AfterTool', 0, ?1, ?2)",
                params![outcome, is_interrupt],
            )
            .unwrap();
        let err = read_events(&tmp.path().join(TRACE_FILE)).unwrap_err();
        assert!(
            matches!(
                err,
                Error::UnknownOutcome { event_id: 1, outcome: ref o, is_interrupt: i }
                    if o.as_deref() == outcome && i == is_interrupt
            ),
            "{outcome:?} {is_interrupt:?}: {err}"
        );
    }
}

/// The table of the issue #3 PR 4 writer (`f95daae`): every AfterTool column but
/// `is_interrupt`.
const PR_4_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    event_id           INTEGER PRIMARY KEY,
    agent_id           TEXT NOT NULL,
    session_id         TEXT NOT NULL,
    vclock             BLOB NOT NULL,
    kind               TEXT NOT NULL,
    tool               TEXT,
    path               TEXT,
    payload_hash       BLOB,
    tool_input         TEXT,
    wall_time          INTEGER NOT NULL,
    tool_use_id        TEXT,
    before_event_id    INTEGER,
    outcome            TEXT,
    input_payload_hash BLOB
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

#[test]
fn a_pr_4_trace_reads_its_successes_unchanged_and_is_not_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("pr4");
    std::fs::create_dir_all(&run_dir).unwrap();
    let conn = Connection::open(run_dir.join(TRACE_FILE)).unwrap();
    conn.execute_batch(PR_4_SCHEMA).unwrap();
    let blobs = trace_store::BlobStore::open(&run_dir).unwrap();
    let response = blobs.put(b"null").unwrap().as_bytes().to_vec();
    let input = blobs.put(INPUT.as_bytes()).unwrap().as_bytes().to_vec();
    conn.execute(
        "INSERT INTO events
            (event_id, agent_id, session_id, vclock, kind, tool, payload_hash, wall_time,
             tool_use_id, before_event_id, outcome, input_payload_hash)
         VALUES (1, 'slot-1', 's', x'0100000000000000', 'AfterTool', 'Bash', ?1, 0,
                 'toolu_1', NULL, 'success', ?2)",
        params![response, input],
    )
    .unwrap();
    drop(conn);
    let bytes = std::fs::read(run_dir.join(TRACE_FILE)).unwrap();
    let cols = columns(&run_dir);
    assert!(!cols.iter().any(|c| c == "is_interrupt"));

    let rows = read_events(&run_dir.join(TRACE_FILE)).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, Some(Outcome::Success));
    assert_eq!(
        read_payload(&run_dir, &rows[0]).unwrap().as_deref(),
        Some(&b"null"[..])
    );
    let err = TraceWriter::create(&run_dir).unwrap_err();
    assert!(matches!(err, Error::RunNotEmpty { events: 1, .. }), "{err}");
    assert_eq!(std::fs::read(run_dir.join(TRACE_FILE)).unwrap(), bytes);
    assert_eq!(columns(&run_dir), cols);
}

#[test]
fn an_empty_pr_4_trace_gets_the_is_interrupt_column() {
    let tmp = tempfile::tempdir().unwrap();
    Connection::open(tmp.path().join(TRACE_FILE))
        .unwrap()
        .execute_batch(PR_4_SCHEMA)
        .unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    assert!(columns(tmp.path()).iter().any(|c| c == "is_interrupt"));
    w.append(&failure(1, r#""x""#, Some(true))).unwrap();
    let row = read_events(&tmp.path().join(TRACE_FILE)).unwrap().remove(0);
    assert_eq!(
        row.outcome,
        Some(Outcome::Failure {
            is_interrupt: Some(true)
        })
    );
    let conn = Connection::open(tmp.path().join(TRACE_FILE)).unwrap();
    assert!(conn
        .execute("UPDATE events SET is_interrupt = 0", [])
        .is_err());
}
