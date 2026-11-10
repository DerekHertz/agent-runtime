//! An event's shape by kind: rows built by the constructors read back as their kind through a
//! real trace, and a row whose columns do not fit its kind is refused with the column named.

use trace_store::{
    read_events, Body, Event, EventKind, Outcome, ShapeError, Stamp, TraceWriter, TRACE_FILE,
};

fn stamp(event_id: u64) -> Stamp {
    Stamp {
        event_id,
        slot: "slot-1".into(),
        session_id: "sess-a".into(),
        vclock: event_id.to_le_bytes().to_vec(),
        wall_time: 0,
    }
}

#[test]
fn constructed_rows_read_back_as_their_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    let failure = Outcome::Failure {
        is_interrupt: Some(true),
    };
    let rows = [
        Event::before_tool(stamp(1), "Edit".into(), "t1".into(), "{}".into(), None),
        Event::after_tool(
            stamp(2),
            "Edit".into(),
            "t1".into(),
            Some(1),
            Outcome::Success,
            "{}".into(),
            "null".into(),
        ),
        Event::after_tool(
            stamp(3),
            "Bash".into(),
            "t2".into(),
            None,
            failure,
            "{}".into(),
            r#""boom""#.into(),
        ),
        Event::stop(stamp(4), "{}".into()),
        Event::session_end(stamp(5), r#"{"reason":"other"}"#.into()),
    ];
    for row in &rows {
        w.append(row).unwrap();
    }
    let read = read_events(&tmp.path().join(TRACE_FILE)).unwrap();
    let bodies: Vec<Body> = read.iter().map(|e| e.body().unwrap()).collect();
    assert_eq!(
        bodies,
        [
            Body::BeforeTool {
                tool: "Edit",
                tool_use_id: Some("t1"),
                path: None
            },
            Body::AfterTool {
                tool: "Edit",
                tool_use_id: Some("t1"),
                before_event_id: Some(1),
                outcome: Outcome::Success
            },
            Body::AfterTool {
                tool: "Bash",
                tool_use_id: Some("t2"),
                before_event_id: None,
                outcome: failure
            },
            Body::Stop,
            Body::SessionEnd,
        ]
    );
}

#[test]
fn a_row_whose_columns_do_not_fit_its_kind_is_refused_by_column() {
    let before = || Event::before_tool(stamp(1), "Edit".into(), "t1".into(), "{}".into(), None);
    let column = |e: Event| match e.body() {
        Err(ShapeError::Missing { column, .. }) => format!("missing {column}"),
        Err(ShapeError::Foreign { column, .. }) => format!("foreign {column}"),
        other => format!("{other:?}"),
    };
    let cases = [
        (
            Event {
                tool: None,
                ..before()
            },
            "missing tool",
        ),
        (
            Event {
                outcome: Some(Outcome::Success),
                ..before()
            },
            "foreign outcome",
        ),
        (
            Event {
                before_event_id: Some(1),
                ..before()
            },
            "foreign before_event_id",
        ),
        (
            Event {
                kind: EventKind::AfterTool,
                ..before()
            },
            "missing outcome",
        ),
        (
            Event {
                tool: Some("Edit".into()),
                ..Event::stop(stamp(1), "{}".into())
            },
            "foreign tool",
        ),
        (
            Event {
                tool_use_id: Some("t1".into()),
                ..Event::session_end(stamp(1), "{}".into())
            },
            "foreign tool_use_id",
        ),
    ];
    for (event, want) in cases {
        assert_eq!(column(event), want);
    }
    let unrecorded = Event {
        kind: EventKind::Acquire,
        ..Event::stop(stamp(7), "{}".into())
    };
    assert_eq!(
        unrecorded.body(),
        Err(ShapeError::NotRecordedYet {
            event_id: 7,
            kind: "Acquire"
        })
    );
}
