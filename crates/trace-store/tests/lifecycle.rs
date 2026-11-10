//! Stop and SessionEnd rows on a real SQLite trace and blob store: the kind is stored as its own
//! text, the payload is the lifecycle fields object stored byte for byte in a blob, and the row
//! carries no tool, `tool_use_id`, pairing, outcome or input blob. A trace whose rows predate the
//! kinds reads unchanged.

use rusqlite::Connection;
use trace_store::{
    read_events, read_input_payload, read_payload, BlobHash, Event, EventKind, TraceWriter,
    BLOBS_DIR, TRACE_FILE,
};

fn lifecycle(id: u64, kind: EventKind, fields: &str) -> Event {
    Event {
        event_id: id,
        slot: "slot-1".into(),
        session_id: "sess-a".into(),
        vclock: id.to_le_bytes().to_vec(),
        kind,
        tool: None,
        path: None,
        payload_hash: None,
        payload: Some(fields.into()),
        input_payload_hash: None,
        input_payload: None,
        tool_use_id: None,
        before_event_id: None,
        outcome: None,
        wall_time: 0,
    }
}

#[test]
fn stop_and_session_end_round_trip_with_their_fields_blob_and_no_tool_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    let stop = r#"{"stop_hook_active":false,"last_assistant_message":"done é\n"}"#;
    let end = r#"{"reason":"other"}"#;
    let empty = "{}";
    w.append(&lifecycle(1, EventKind::Stop, stop)).unwrap();
    w.append(&lifecycle(2, EventKind::SessionEnd, end)).unwrap();
    // Equal fields share one blob, whatever the kind.
    w.append(&lifecycle(3, EventKind::Stop, end)).unwrap();
    w.append(&lifecycle(4, EventKind::SessionEnd, empty))
        .unwrap();

    let rows = read_events(&tmp.path().join(TRACE_FILE)).unwrap();
    let kinds: Vec<EventKind> = rows.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [
            EventKind::Stop,
            EventKind::SessionEnd,
            EventKind::Stop,
            EventKind::SessionEnd
        ]
    );
    for (row, fields) in rows.iter().zip([stop, end, end, empty]) {
        assert_eq!(
            read_payload(tmp.path(), row).unwrap().as_deref(),
            Some(fields.as_bytes()),
            "row {}",
            row.event_id
        );
        assert_eq!(
            row.payload_hash.as_deref(),
            Some(&BlobHash::of(fields.as_bytes()).as_bytes()[..])
        );
        assert_eq!(read_input_payload(tmp.path(), row).unwrap(), None);
        assert_eq!(
            (
                &row.tool,
                &row.tool_use_id,
                row.before_event_id,
                row.outcome,
                &row.payload
            ),
            (&None, &None, None, None, &None)
        );
    }
    assert_eq!(rows[1].payload_hash, rows[2].payload_hash);
    let blobs = std::fs::read_dir(tmp.path().join(BLOBS_DIR))
        .unwrap()
        .count();
    assert_eq!(blobs, 3, "stop, end and {{}}: equal fields share a blob");

    // The kind column holds the plain names, readable without the typed reader.
    let conn = Connection::open(tmp.path().join(TRACE_FILE)).unwrap();
    let mut stmt = conn
        .prepare("SELECT kind, tool, tool_use_id, outcome, is_interrupt, input_payload_hash FROM events ORDER BY event_id")
        .unwrap();
    type Raw = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<Vec<u8>>,
    );
    let raw: Vec<Raw> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let names: Vec<&str> = raw.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(names, ["Stop", "SessionEnd", "Stop", "SessionEnd"]);
    assert!(raw.iter().all(|r| r.1.is_none()
        && r.2.is_none()
        && r.3.is_none()
        && r.4.is_none()
        && r.5.is_none()));
}

#[test]
fn an_unknown_kind_in_a_stored_row_is_an_error_not_a_guess() {
    let tmp = tempfile::tempdir().unwrap();
    drop(TraceWriter::create(tmp.path()).unwrap());
    Connection::open(tmp.path().join(TRACE_FILE))
        .unwrap()
        .execute(
            "INSERT INTO events (event_id, agent_id, session_id, vclock, kind, wall_time)
             VALUES (1, 'slot-1', 's', x'', 'stop', 0)",
            [],
        )
        .unwrap();
    let err = read_events(&tmp.path().join(TRACE_FILE)).unwrap_err();
    assert!(err.to_string().contains("unknown event kind"), "{err}");
}
