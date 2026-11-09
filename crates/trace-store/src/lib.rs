//! Trace store: the append-only SQLite trace of stamped events, one database per run
//! (SPEC section 5, "Event schema" and "Layout").
//!
//! The coordinator is the only writer. This slice has a writer, a minimal in-order reader for
//! tests, and the run's blob store ([`BlobStore`]); the in-memory query layer (`Run::load`,
//! `happens_before`, ...) arrives with a later issue.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};

mod blobs;
pub use blobs::{BlobError, BlobHash, BlobStore, BLOBS_DIR};

/// File name of the trace inside a run directory.
pub const TRACE_FILE: &str = "trace.db";

/// Lock file inside a run directory, held by the run's single trace writer.
pub const LOCK_FILE: &str = "trace.lock";

/// What kind of happening an [`Event`] records (SPEC section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    BeforeTool,
    AfterTool,
    Acquire,
    Release,
    Invalidate,
    Barrier,
    Scatter,
    Gather,
}

impl EventKind {
    /// The text stored in the `kind` column.
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::BeforeTool => "BeforeTool",
            EventKind::AfterTool => "AfterTool",
            EventKind::Acquire => "Acquire",
            EventKind::Release => "Release",
            EventKind::Invalidate => "Invalidate",
            EventKind::Barrier => "Barrier",
            EventKind::Scatter => "Scatter",
            EventKind::Gather => "Gather",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "BeforeTool" => EventKind::BeforeTool,
            "AfterTool" => EventKind::AfterTool,
            "Acquire" => EventKind::Acquire,
            "Release" => EventKind::Release,
            "Invalidate" => EventKind::Invalidate,
            "Barrier" => EventKind::Barrier,
            "Scatter" => EventKind::Scatter,
            "Gather" => EventKind::Gather,
            _ => return None,
        })
    }
}

/// One row of the trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Monotonic per coordinator. Numbers events for storage; carries no causal meaning.
    pub event_id: u64,
    /// Stored in the SPEC section 5 column `agent_id`.
    pub slot: String,
    pub session_id: String,
    /// The slot's full clock, as `clock::Clock::to_bytes`.
    pub vclock: Vec<u8>,
    pub kind: EventKind,
    pub tool: Option<String>,
    pub path: Option<String>,
    pub payload_hash: Option<Vec<u8>>,
    /// The raw hook `tool_input` JSON, exactly as the shim forwarded it, so a run can be
    /// re-classified offline (SPEC section 7). Kept inline until the blob store arrives; then the
    /// content moves to `blobs/` and `payload_hash` references it.
    pub tool_input: Option<String>,
    /// Milliseconds since the Unix epoch. Display only, never for ordering.
    pub wall_time: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("trace io error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("trace database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("event_id {0} does not fit the trace's signed 64-bit column")]
    EventIdRange(u64),
    #[error("trace row {event_id} has unknown event kind {kind:?}")]
    UnknownKind { event_id: i64, kind: String },
    #[error(
        "{path} already holds {events} events; resuming a run is not supported, start a new run id"
    )]
    RunNotEmpty { path: PathBuf, events: u64 },
    #[error("run {path} is already held by another coordinator")]
    RunInUse { path: PathBuf },
}

// event_id is the primary key, so a duplicate id fails. UPDATE and DELETE are refused by
// triggers, so the table is append-only even for a stray connection.
const SCHEMA: &str = "
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

/// The single writer of a run's trace. Append-only: there is no update or delete.
#[derive(Debug)]
pub struct TraceWriter {
    conn: Connection,
    /// Holds an exclusive lock on `run_dir/trace.lock` for the writer's lifetime, so a second
    /// coordinator on the same run is refused even before either has recorded an event.
    _lock: File,
}

impl TraceWriter {
    /// Open `run_dir/trace.db`, creating the directory and the schema if absent. Refused while
    /// another writer holds the run, and refused if the trace already holds events: the
    /// coordinator's clocks and event ids start from zero, so writing on would restamp history.
    pub fn create(run_dir: &Path) -> Result<Self, Error> {
        std::fs::create_dir_all(run_dir).map_err(|source| Error::Io {
            path: run_dir.to_path_buf(),
            source,
        })?;
        let lock_path = run_dir.join(LOCK_FILE);
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|source| Error::Io {
                path: lock_path.clone(),
                source,
            })?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(Error::RunInUse {
                    path: run_dir.to_path_buf(),
                })
            }
            Err(TryLockError::Error(source)) => {
                return Err(Error::Io {
                    path: lock_path,
                    source,
                })
            }
        }
        let path = run_dir.join(TRACE_FILE);
        let conn = Connection::open(&path)?;
        conn.execute_batch(SCHEMA)?;
        let events: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
        if events > 0 {
            return Err(Error::RunNotEmpty {
                path,
                events: events as u64,
            });
        }
        Ok(Self { conn, _lock: lock })
    }

    /// Append one event. Each call is its own transaction, so a returned `Ok` is durable
    /// to SQLite's default guarantees. Fails if `event_id` already exists.
    pub fn append(&mut self, event: &Event) -> Result<(), Error> {
        let id = to_row_id(event.event_id)?;
        self.conn.execute(
            "INSERT INTO events
                (event_id, agent_id, session_id, vclock, kind, tool, path, payload_hash,
                 tool_input, wall_time)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                event.slot,
                event.session_id,
                event.vclock,
                event.kind.as_str(),
                event.tool,
                event.path,
                event.payload_hash,
                event.tool_input,
                event.wall_time,
            ],
        )?;
        Ok(())
    }
}

fn to_row_id(event_id: u64) -> Result<i64, Error> {
    i64::try_from(event_id).map_err(|_| Error::EventIdRange(event_id))
}

type Row = (
    i64,
    String,
    String,
    Vec<u8>,
    String,
    Option<String>,
    Option<String>,
    Option<Vec<u8>>,
    Option<String>,
    i64,
);

/// Read every event in `event_id` order. A minimal reader for tests and tooling; the real query
/// layer is a later issue.
pub fn read_events(db_path: &Path) -> Result<Vec<Event>, Error> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT event_id, agent_id, session_id, vclock, kind, tool, path, payload_hash,
                tool_input, wall_time
         FROM events ORDER BY event_id",
    )?;
    let rows = stmt.query_map([], |row| -> rusqlite::Result<Row> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
        ))
    })?;
    let mut events = Vec::new();
    for row in rows {
        let (id, slot, session_id, vclock, kind, tool, path, payload_hash, tool_input, wall_time) =
            row?;
        let kind = EventKind::parse(&kind).ok_or(Error::UnknownKind { event_id: id, kind })?;
        events.push(Event {
            event_id: id as u64,
            slot,
            session_id,
            vclock,
            kind,
            tool,
            path,
            payload_hash,
            tool_input,
            wall_time,
        });
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: u64, kind: EventKind) -> Event {
        Event {
            event_id: id,
            slot: "slot-1".into(),
            session_id: "sess-a".into(),
            vclock: vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            kind,
            tool: Some("Edit".into()),
            path: None,
            payload_hash: None,
            tool_input: None,
            wall_time: 1_700_000_000_123,
        }
    }

    #[test]
    fn create_makes_missing_run_dir_and_db() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("runs").join("r1");
        TraceWriter::create(&run_dir).unwrap();
        assert!(run_dir.join(TRACE_FILE).is_file());
        assert!(read_events(&run_dir.join(TRACE_FILE)).unwrap().is_empty());
    }

    #[test]
    fn appended_events_round_trip_in_event_id_order() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = TraceWriter::create(tmp.path()).unwrap();
        let mut full = event(2, EventKind::AfterTool);
        full.path = Some("src/lib.rs".into());
        full.payload_hash = Some(vec![9; 32]);
        full.tool_input = Some(r#"{"file_path":"src/lib.rs", "x":1}"#.into());
        w.append(&full).unwrap();
        w.append(&event(1, EventKind::BeforeTool)).unwrap();

        let got = read_events(&tmp.path().join(TRACE_FILE)).unwrap();
        assert_eq!(got, vec![event(1, EventKind::BeforeTool), full]);
    }

    #[test]
    fn every_kind_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = TraceWriter::create(tmp.path()).unwrap();
        let kinds = [
            EventKind::BeforeTool,
            EventKind::AfterTool,
            EventKind::Acquire,
            EventKind::Release,
            EventKind::Invalidate,
            EventKind::Barrier,
            EventKind::Scatter,
            EventKind::Gather,
        ];
        for (i, k) in kinds.iter().enumerate() {
            w.append(&event(i as u64, *k)).unwrap();
        }
        let got: Vec<EventKind> = read_events(&tmp.path().join(TRACE_FILE))
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(got, kinds);
    }

    #[test]
    fn duplicate_event_id_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = TraceWriter::create(tmp.path()).unwrap();
        w.append(&event(1, EventKind::BeforeTool)).unwrap();
        assert!(w.append(&event(1, EventKind::BeforeTool)).is_err());
        assert_eq!(read_events(&tmp.path().join(TRACE_FILE)).unwrap().len(), 1);
    }

    #[test]
    fn event_id_beyond_i64_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = TraceWriter::create(tmp.path()).unwrap();
        let err = w
            .append(&event(u64::MAX, EventKind::BeforeTool))
            .unwrap_err();
        assert!(matches!(err, Error::EventIdRange(_)));
    }

    #[test]
    fn reopening_an_empty_trace_works_but_a_nonempty_one_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        drop(TraceWriter::create(tmp.path()).unwrap());
        TraceWriter::create(tmp.path())
            .unwrap()
            .append(&event(1, EventKind::BeforeTool))
            .unwrap();
        let err = TraceWriter::create(tmp.path()).unwrap_err();
        assert!(matches!(err, Error::RunNotEmpty { events: 1, .. }), "{err}");
        assert_eq!(read_events(&tmp.path().join(TRACE_FILE)).unwrap().len(), 1);
    }

    #[test]
    fn a_second_writer_on_a_held_run_is_refused_even_before_any_event() {
        let tmp = tempfile::tempdir().unwrap();
        let first = TraceWriter::create(tmp.path()).unwrap();
        let err = TraceWriter::create(tmp.path()).unwrap_err();
        assert!(matches!(err, Error::RunInUse { .. }), "{err}");
        drop(first);
        // Released with the writer: an empty run can be taken again.
        TraceWriter::create(tmp.path()).unwrap();
    }

    #[test]
    fn rows_cannot_be_updated_or_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = TraceWriter::create(tmp.path()).unwrap();
        w.append(&event(1, EventKind::BeforeTool)).unwrap();
        assert!(w.conn.execute("UPDATE events SET tool = 'x'", []).is_err());
        assert!(w.conn.execute("DELETE FROM events", []).is_err());
        assert_eq!(read_events(&tmp.path().join(TRACE_FILE)).unwrap().len(), 1);
    }
}
