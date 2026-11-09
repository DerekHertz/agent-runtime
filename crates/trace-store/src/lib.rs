//! Trace store: the append-only SQLite trace of stamped events, one database per run
//! (SPEC section 5, "Event schema" and "Layout").
//!
//! The coordinator is the only writer. This slice has a writer, a minimal in-order reader for
//! tests and tooling ([`read_events`], [`read_payload`]), and the run's blob store
//! ([`BlobStore`]); the in-memory query layer (`Run::load`, `happens_before`, ...) arrives with a
//! later issue.
//!
//! Payloads: [`TraceWriter::append`] stores an event's raw tool input as a blob in
//! `runs/<run-id>/blobs/` and only then inserts the row, which keeps the blob's hash in
//! `payload_hash` and leaves `tool_input` NULL. So a row never names a blob that is missing or
//! not yet flushed; an append whose INSERT fails can leave an orphan blob, which nothing reads.
//! Traces written before blobs (issue #2) keep the raw input inline in `tool_input`;
//! [`read_payload`] reads either form.
//!
//! File-system assumptions: when the writer starts, it flushes the run directory and every
//! directory on its canonical path up to its filesystem's anchor (the topmost directory on the
//! same device), whether or not it created them. Blob durability relies on the OS and drive honouring `fsync` (`F_FULLFSYNC`
//! on macOS), as described in the `blobs` module. Rows rely on SQLite's default durability.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};

mod blobs;
mod durable_dir;
pub use blobs::{read_blob, BlobError, BlobHash, BlobStore, BLOBS_DIR};

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
    /// The full 32-byte blake3 hash of the event's payload, stored in the run's `blobs/`. Only
    /// [`TraceWriter::append`] sets it: an event handed to `append` with a hash is refused, so
    /// every hash in the trace names a blob the writer stored and flushed first.
    pub payload_hash: Option<Vec<u8>>,
    /// The raw hook `tool_input` JSON, exactly as the shim forwarded it, so a run can be
    /// re-classified offline (SPEC section 7).
    ///
    /// Given to [`TraceWriter::append`], it is the payload to store: the writer moves it into a
    /// blob and the row keeps only `payload_hash`. Read back with [`read_events`], it is `Some`
    /// only for rows written before blobs (issue #2 traces), which kept it inline. Use
    /// [`read_payload`] to get the bytes from either form.
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
    #[error("payload blob error: {0}")]
    Blob(#[from] BlobError),
    #[error("event {event_id} arrived with a payload_hash; only the writer may set one")]
    PayloadHashSupplied { event_id: u64 },
    #[error("trace row {event_id} has a {len}-byte payload_hash, not 32 bytes")]
    BadPayloadHash { event_id: u64, len: usize },
    #[error("trace row {event_id} has both a payload_hash and an inline tool_input")]
    TwoPayloads { event_id: u64 },
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

/// The single writer of a run's trace and its payload blobs. Append-only: there is no update or
/// delete.
#[derive(Debug)]
pub struct TraceWriter {
    conn: Connection,
    blobs: BlobStore,
    /// Holds an exclusive lock on `run_dir/trace.lock` for the writer's lifetime, so a second
    /// coordinator on the same run is refused even before either has recorded an event.
    _lock: File,
}

impl TraceWriter {
    /// Open `run_dir/trace.db` and `run_dir/blobs/`, creating the directory and the schema if
    /// absent. The run directory and each directory above it up to its filesystem's anchor are
    /// flushed, even ones that already existed, since a concurrent start may have created them
    /// without flushing yet. So the run directory is durable before any blob is stored in it (the
    /// [`BlobStore`] contract).
    /// Refused while another writer holds the run, and refused if the trace already holds events:
    /// the coordinator's clocks and event ids start from zero, so writing on would restamp
    /// history. A refused run gets no `blobs/` directory.
    pub fn create(run_dir: &Path) -> Result<Self, Error> {
        durable_dir::create(run_dir).map_err(|(path, source)| Error::Io { path, source })?;
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
        let blobs = BlobStore::open(run_dir)?;
        Ok(Self {
            conn,
            blobs,
            _lock: lock,
        })
    }

    /// Append one event. If it carries a `tool_input`, those exact bytes are first stored and
    /// flushed as a blob; the row then holds the blob's hash in `payload_hash` and NULL in
    /// `tool_input`. If storing the blob fails, nothing is inserted. If the INSERT fails, the
    /// stored blob stays as an orphan that no row names; storing the same bytes again reuses it.
    ///
    /// Each call is its own transaction, so a returned `Ok` is durable to SQLite's default
    /// guarantees. Fails if `event_id` already exists, or if the event already has a
    /// `payload_hash` (only this writer may set one).
    pub fn append(&mut self, event: &Event) -> Result<(), Error> {
        let id = to_row_id(event.event_id)?;
        if event.payload_hash.is_some() {
            return Err(Error::PayloadHashSupplied {
                event_id: event.event_id,
            });
        }
        let payload_hash = match &event.tool_input {
            Some(input) => Some(self.blobs.put(input.as_bytes())?.as_bytes().to_vec()),
            None => None,
        };
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
                payload_hash,
                Option::<String>::None,
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

/// The exact payload bytes of `event`, a row read from the trace in `run_dir`: from its blob when
/// it has a `payload_hash`, from the inline `tool_input` column for a row written before blobs
/// (an issue #2 trace), and `None` when it has neither. The blob is rehashed on read, so a
/// missing or corrupt blob is an error, never wrong bytes. Reading creates nothing in `run_dir`.
pub fn read_payload(run_dir: &Path, event: &Event) -> Result<Option<Vec<u8>>, Error> {
    match (&event.payload_hash, &event.tool_input) {
        (Some(_), Some(_)) => Err(Error::TwoPayloads {
            event_id: event.event_id,
        }),
        (Some(hash), None) => {
            let hash: [u8; 32] = hash
                .as_slice()
                .try_into()
                .map_err(|_| Error::BadPayloadHash {
                    event_id: event.event_id,
                    len: hash.len(),
                })?;
            Ok(Some(read_blob(run_dir, &BlobHash::from(hash))?))
        }
        (None, Some(inline)) => Ok(Some(inline.clone().into_bytes())),
        (None, None) => Ok(None),
    }
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
        let raw = r#"{"file_path":"src/lib.rs", "x":1}"#;
        let mut full = event(2, EventKind::AfterTool);
        full.path = Some("src/lib.rs".into());
        full.tool_input = Some(raw.into());
        w.append(&full).unwrap();
        w.append(&event(1, EventKind::BeforeTool)).unwrap();

        let got = read_events(&tmp.path().join(TRACE_FILE)).unwrap();
        // The row keeps the payload's hash, not the payload.
        let mut stored = full.clone();
        stored.tool_input = None;
        stored.payload_hash = Some(BlobHash::of(raw.as_bytes()).as_bytes().to_vec());
        assert_eq!(got, vec![event(1, EventKind::BeforeTool), stored]);
        assert_eq!(read_payload(tmp.path(), &got[0]).unwrap(), None);
        assert_eq!(
            read_payload(tmp.path(), &got[1]).unwrap().as_deref(),
            Some(raw.as_bytes())
        );
    }

    #[test]
    fn an_event_with_a_caller_supplied_hash_is_refused_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = TraceWriter::create(tmp.path()).unwrap();
        let mut forged = event(1, EventKind::BeforeTool);
        forged.payload_hash = Some(vec![7; 32]);
        let err = w.append(&forged).unwrap_err();
        assert!(
            matches!(err, Error::PayloadHashSupplied { event_id: 1 }),
            "{err}"
        );
        assert!(read_events(&tmp.path().join(TRACE_FILE))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_refused_run_gets_no_blobs_directory() {
        let tmp = tempfile::tempdir().unwrap();
        drop(TraceWriter::create(tmp.path()).unwrap());
        std::fs::remove_dir(tmp.path().join(BLOBS_DIR)).unwrap();
        let held = TraceWriter::create(tmp.path()).unwrap();
        std::fs::remove_dir(tmp.path().join(BLOBS_DIR)).unwrap();
        assert!(matches!(
            TraceWriter::create(tmp.path()).unwrap_err(),
            Error::RunInUse { .. }
        ));
        assert!(!tmp.path().join(BLOBS_DIR).exists());
        drop(held);
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
