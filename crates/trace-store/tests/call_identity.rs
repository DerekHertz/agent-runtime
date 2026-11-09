//! The `tool_use_id` column on a real SQLite trace: new rows keep it, and traces written before it
//! existed (issue #2, with inline input, and issue #3 PR 2, with blobs) are read as they are, with
//! `tool_use_id` `None`, and are never changed by a reader or by a refused writer.

use std::path::Path;

use rusqlite::{params, Connection};
use trace_store::{
    read_events, read_payload, BlobStore, Error, Event, EventKind, TraceWriter, TRACE_FILE,
};

fn event(id: u64, tool_use_id: Option<&str>) -> Event {
    Event {
        event_id: id,
        slot: "slot-1".into(),
        session_id: "sess-a".into(),
        vclock: id.to_le_bytes().to_vec(),
        kind: EventKind::BeforeTool,
        tool: Some("Edit".into()),
        path: None,
        payload_hash: None,
        tool_input: Some(format!(r#"{{"call":{id}}}"#)),
        tool_use_id: tool_use_id.map(str::to_owned),
        wall_time: 0,
    }
}

/// The table and triggers shared by the issue #2 writer (`6169b5f`) and the issue #3 PR 2 writer
/// (`e813745`): no `tool_use_id` column.
const PRE_ID_SCHEMA: &str = "
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

fn pre_id_trace(run_dir: &Path) -> Connection {
    std::fs::create_dir_all(run_dir).unwrap();
    let conn = Connection::open(run_dir.join(TRACE_FILE)).unwrap();
    conn.execute_batch(PRE_ID_SCHEMA).unwrap();
    conn
}

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

fn has_column(run_dir: &Path) -> bool {
    columns(run_dir).iter().any(|c| c == "tool_use_id")
}

fn file_bytes(run_dir: &Path) -> Vec<u8> {
    std::fs::read(run_dir.join(TRACE_FILE)).unwrap()
}

#[test]
fn a_new_row_keeps_its_tool_use_id_and_an_event_without_one_stores_null() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    w.append(&event(1, Some("toolu_01Uy28i9ZxHpdQkCRWhbwK3g")))
        .unwrap();
    w.append(&event(2, None)).unwrap();

    let rows = read_events(&tmp.path().join(TRACE_FILE)).unwrap();
    assert_eq!(
        rows[0].tool_use_id.as_deref(),
        Some("toolu_01Uy28i9ZxHpdQkCRWhbwK3g")
    );
    assert_eq!(rows[1].tool_use_id, None);
    let stored: Option<String> = Connection::open(tmp.path().join(TRACE_FILE))
        .unwrap()
        .query_row(
            "SELECT tool_use_id FROM events WHERE event_id = 2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, None, "NULL in the database, not an empty string");
}

#[test]
fn an_issue_2_trace_reads_with_no_tool_use_id_and_is_not_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("old");
    let raw = r#"{"file_path":"/w/a.rs"}"#;
    let conn = pre_id_trace(&run_dir);
    conn.execute(
        "INSERT INTO events
            (event_id, agent_id, session_id, vclock, kind, tool, path, payload_hash,
             tool_input, wall_time)
         VALUES (1, 'slot-1', 's', x'0100000000000000', 'BeforeTool', 'Edit', NULL, NULL,
                 ?1, 0)",
        params![raw],
    )
    .unwrap();
    drop(conn);
    let before = (file_bytes(&run_dir), columns(&run_dir));

    let rows = read_events(&run_dir.join(TRACE_FILE)).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].tool_use_id, None, "no id is invented");
    assert_eq!(
        read_payload(&run_dir, &rows[0]).unwrap().as_deref(),
        Some(raw.as_bytes())
    );

    // Not a resume path: the writer refuses it before touching the schema.
    let err = TraceWriter::create(&run_dir).unwrap_err();
    assert!(matches!(err, Error::RunNotEmpty { events: 1, .. }), "{err}");
    assert_eq!((file_bytes(&run_dir), columns(&run_dir)), before);
    assert!(!has_column(&run_dir));
}

#[test]
fn a_pr_2_trace_with_blobs_reads_with_no_tool_use_id_and_is_not_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("pr2");
    let conn = pre_id_trace(&run_dir);
    let raw = r#"{"command":"cargo test"}"#;
    let hash = BlobStore::open(&run_dir)
        .unwrap()
        .put(raw.as_bytes())
        .unwrap();
    conn.execute(
        "INSERT INTO events
            (event_id, agent_id, session_id, vclock, kind, tool, path, payload_hash,
             tool_input, wall_time)
         VALUES (1, 'slot-1', 's', x'0100000000000000', 'BeforeTool', 'Bash', NULL, ?1,
                 NULL, 0)",
        params![hash.as_bytes().to_vec()],
    )
    .unwrap();
    drop(conn);
    let before = (file_bytes(&run_dir), columns(&run_dir));

    let rows = read_events(&run_dir.join(TRACE_FILE)).unwrap();
    assert_eq!(rows[0].tool_use_id, None);
    assert_eq!(
        read_payload(&run_dir, &rows[0]).unwrap().as_deref(),
        Some(raw.as_bytes())
    );
    let err = TraceWriter::create(&run_dir).unwrap_err();
    assert!(matches!(err, Error::RunNotEmpty { events: 1, .. }), "{err}");
    assert_eq!((file_bytes(&run_dir), columns(&run_dir)), before);
    assert!(!has_column(&run_dir));
}

#[test]
fn an_empty_trace_from_before_the_column_gets_it_and_then_keeps_ids() {
    let tmp = tempfile::tempdir().unwrap();
    drop(pre_id_trace(tmp.path()));
    assert!(!has_column(tmp.path()));

    let mut w = TraceWriter::create(tmp.path()).unwrap();
    assert!(has_column(tmp.path()));
    w.append(&event(1, Some("toolu_x"))).unwrap();
    let rows = read_events(&tmp.path().join(TRACE_FILE)).unwrap();
    assert_eq!(rows[0].tool_use_id.as_deref(), Some("toolu_x"));
    // The append-only triggers still hold after the column was added.
    let conn = Connection::open(tmp.path().join(TRACE_FILE)).unwrap();
    assert!(conn
        .execute("UPDATE events SET tool_use_id = 'y'", [])
        .is_err());
    assert!(conn.execute("DELETE FROM events", []).is_err());
}
