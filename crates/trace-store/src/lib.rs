//! Trace store: the append-only SQLite trace of stamped events, one database per run
//! (SPEC section 5, "Event schema" and "Layout").
//!
//! The coordinator is the only writer. This slice has a writer, a minimal in-order reader for
//! tests and tooling ([`read_events`], [`read_payload`], [`read_input_payload`]), and the run's
//! blob store ([`BlobStore`]); the in-memory query layer (`Run::load`, `happens_before`, ...)
//! arrives with a later issue.
//!
//! Payloads: an event handed to [`TraceWriter::append`] carries its payload bytes, and the writer
//! stores them as blobs in `runs/<run-id>/blobs/` and only then inserts the row, which keeps only
//! their hashes. A BeforeTool's payload is its raw tool input. A successful AfterTool's payload
//! is its raw tool response and a failed one's the raw JSON text of its hook `error`; either way
//! the raw tool input reported with it is a second blob named by `input_payload_hash`, so a
//! completion whose BeforeTool was never recorded still keeps its input. So a row never names a blob that is missing or not yet flushed; an append whose INSERT
//! fails can leave orphan blobs, which nothing reads. Traces written before blobs (issue #2) keep
//! the raw input inline in the `tool_input` column; [`read_payload`] reads either form.
//!
//! Columns added after issue #2: `tool_use_id` (issue #3 PR 3), then `before_event_id`, `outcome`
//! and `input_payload_hash` (issue #3 PR 4, AfterTool), then `is_interrupt` (issue #3 PR 5,
//! failed AfterTool). [`read_events`] reads a trace that lacks
//! any of them with `None` in its place and never adds a column. A writer adds them only to an
//! empty trace.
//!
//! File-system assumptions: when the writer starts, it flushes the run directory and every
//! directory on its canonical path up to its filesystem's anchor (the topmost directory on the
//! same device), whether or not it created them. Blob durability relies on the OS and drive
//! honouring `fsync` (`F_FULLFSYNC` on macOS), as described in the `blobs` module. Rows rely on
//! SQLite's default durability.

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
///
/// `Stop` and `SessionEnd` are lifecycle reports (issue #3 PR 6): a session's Stop or SessionEnd
/// hook, recorded as an event of its slot. They carry no tool, no `tool_use_id` and no outcome,
/// and they release nothing: a lease release is its own `Release` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    BeforeTool,
    AfterTool,
    Stop,
    SessionEnd,
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
            EventKind::Stop => "Stop",
            EventKind::SessionEnd => "SessionEnd",
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
            "Stop" => EventKind::Stop,
            "SessionEnd" => EventKind::SessionEnd,
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

/// How a tool call ended, as its AfterTool reports it.
///
/// Stored in two columns: `outcome` holds [`Outcome::as_str`], and `is_interrupt` holds a
/// failure's interruption flag as 1 or 0, or NULL when the report omitted it (and for every
/// success). The NULL keeps an omitted flag apart from an explicit `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// PostToolUse: the tool ran and returned a response.
    Success,
    /// PostToolUseFailure: the call failed or was interrupted. `is_interrupt` is the hook's flag
    /// as reported, `None` when the hook input did not carry it.
    Failure { is_interrupt: Option<bool> },
}

impl Outcome {
    /// The text stored in the `outcome` column.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::Failure { .. } => "failure",
        }
    }

    /// The value stored in the `is_interrupt` column: NULL for a success, and for a failure
    /// whose report omitted the flag.
    pub fn is_interrupt(self) -> Option<bool> {
        match self {
            Outcome::Success => None,
            Outcome::Failure { is_interrupt } => is_interrupt,
        }
    }

    /// The outcome stored as `text` in the `outcome` column and `is_interrupt` in the
    /// `is_interrupt` column, or `None` when [`TraceWriter::append`] never writes that pair.
    pub fn from_columns(text: &str, is_interrupt: Option<i64>) -> Option<Self> {
        let flag = match is_interrupt {
            None => None,
            Some(0) => Some(false),
            Some(1) => Some(true),
            Some(_) => return None,
        };
        match (text, flag) {
            ("success", None) => Some(Outcome::Success),
            ("failure", is_interrupt) => Some(Outcome::Failure { is_interrupt }),
            _ => None,
        }
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
    /// The event's payload, exactly as the shim forwarded it: a BeforeTool's raw hook
    /// `tool_input` (kept so a run can be re-classified offline, SPEC section 7), a successful
    /// AfterTool's raw hook `tool_response`, a failed AfterTool's raw hook `error` (the JSON text
    /// of a string, quotes and escapes included; never a synthesized envelope), and a Stop's or
    /// SessionEnd's lifecycle fields: a JSON object holding exactly the fields the hook carried,
    /// each value its raw JSON text (SPEC section 5).
    ///
    /// Given to [`TraceWriter::append`], it is the payload to store: the writer moves it into a
    /// blob and the row keeps only `payload_hash`. Read back with [`read_events`], it is `Some`
    /// only for rows written before blobs (issue #2 traces), which kept the tool input inline in
    /// the `tool_input` column. Use [`read_payload`] to get the bytes from either form.
    pub payload: Option<String>,
    /// The hash of the raw `tool_input` an AfterTool reported, stored like `payload_hash` and set
    /// only by [`TraceWriter::append`]. It is separate from `payload_hash` so the input survives
    /// even when no BeforeTool was recorded, and so the input of a paired call is stored once.
    pub input_payload_hash: Option<Vec<u8>>,
    /// Given to [`TraceWriter::append`]: an AfterTool's raw hook `tool_input`, which the writer
    /// stores as a blob named by `input_payload_hash`. Always `None` when read back; use
    /// [`read_input_payload`].
    pub input_payload: Option<String>,
    /// Claude Code's `tool_use_id` for a tool event; with `session_id` it names the call. `None`
    /// for events without one and for every row of a trace written before the column existed.
    pub tool_use_id: Option<String>,
    /// For an AfterTool, the `event_id` of the BeforeTool of the same call, `None` when the
    /// coordinator recorded no BeforeTool for it (an orphan completion). `None` for every other
    /// kind and for traces written before the column existed.
    pub before_event_id: Option<u64>,
    /// For an AfterTool, how the call ended, with a failure's interruption flag (see
    /// [`Outcome`]). `None` for every other kind and for traces written before the column
    /// existed.
    pub outcome: Option<Outcome>,
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
    #[error("event id {0} does not fit the trace's signed 64-bit column")]
    EventIdRange(u64),
    #[error("trace row {event_id} has unknown event kind {kind:?}")]
    UnknownKind { event_id: i64, kind: String },
    #[error(
        "trace row {event_id} has unknown outcome {outcome:?} with is_interrupt {is_interrupt:?}"
    )]
    UnknownOutcome {
        event_id: i64,
        outcome: Option<String>,
        is_interrupt: Option<i64>,
    },
    #[error(
        "{path} already holds {events} events; resuming a run is not supported, start a new run id"
    )]
    RunNotEmpty { path: PathBuf, events: u64 },
    #[error("run {path} is already held by another coordinator")]
    RunInUse { path: PathBuf },
    #[error("payload blob error: {0}")]
    Blob(#[from] BlobError),
    #[error("event {event_id} arrived with a payload hash; only the writer may set one")]
    PayloadHashSupplied { event_id: u64 },
    #[error("trace row {event_id} has a {len}-byte payload hash, not 32 bytes")]
    BadPayloadHash { event_id: u64, len: usize },
    #[error("trace row {event_id} has both a payload_hash and an inline tool_input")]
    TwoPayloads { event_id: u64 },
}

// event_id is the primary key, so a duplicate id fails. UPDATE and DELETE are refused by
// triggers, so the table is append-only even for a stray connection.
const SCHEMA: &str = "
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
    input_payload_hash BLOB,
    is_interrupt       INTEGER
);
CREATE TRIGGER IF NOT EXISTS events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'trace is append-only'); END;
CREATE TRIGGER IF NOT EXISTS events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'trace is append-only'); END;
";

/// Nullable columns added after issue #2, in the order they were added, with their types. A
/// trace written before one of them reads it as NULL; an empty such trace gets it from a writer.
const LATER_COLUMNS: [(&str, &str); 5] = [
    ("tool_use_id", "TEXT"),
    ("before_event_id", "INTEGER"),
    ("outcome", "TEXT"),
    ("input_payload_hash", "BLOB"),
    ("is_interrupt", "INTEGER"),
];

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
    /// history. A refused run gets no `blobs/` directory and no schema change. An empty trace from
    /// before a later column existed gets that column.
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
        for (name, sql_type) in LATER_COLUMNS {
            if !has_column(&conn, name)? {
                // An empty trace created before the column existed. Nothing in it needs the
                // column, so adding it changes no recorded event.
                conn.execute_batch(&format!("ALTER TABLE events ADD COLUMN {name} {sql_type}"))?;
            }
        }
        let blobs = BlobStore::open(run_dir)?;
        Ok(Self {
            conn,
            blobs,
            _lock: lock,
        })
    }

    /// Append one event. Its `payload` and then its `input_payload`, when present, are first
    /// stored and flushed as blobs; the row then holds their hashes in `payload_hash` and
    /// `input_payload_hash`, and NULL in the inline `tool_input` column. If storing a blob fails,
    /// nothing is inserted. If the INSERT (or the second blob) fails, the blobs already stored
    /// stay as orphans that no row names; storing the same bytes again reuses them.
    ///
    /// Each call is its own transaction, so a returned `Ok` is durable to SQLite's default
    /// guarantees. Fails if `event_id` already exists, or if the event already has a payload hash
    /// (only this writer may set one).
    pub fn append(&mut self, event: &Event) -> Result<(), Error> {
        let id = to_row_id(event.event_id)?;
        let before_event_id = event.before_event_id.map(to_row_id).transpose()?;
        if event.payload_hash.is_some() || event.input_payload_hash.is_some() {
            return Err(Error::PayloadHashSupplied {
                event_id: event.event_id,
            });
        }
        let payload_hash = self.put(event.payload.as_deref())?;
        let input_payload_hash = self.put(event.input_payload.as_deref())?;
        self.conn.execute(
            "INSERT INTO events
                (event_id, agent_id, session_id, vclock, kind, tool, path, payload_hash,
                 tool_input, wall_time, tool_use_id, before_event_id, outcome,
                 input_payload_hash, is_interrupt)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                id,
                event.slot,
                event.session_id,
                event.vclock,
                event.kind.as_str(),
                event.tool,
                event.path,
                payload_hash,
                event.wall_time,
                event.tool_use_id,
                before_event_id,
                event.outcome.map(Outcome::as_str),
                input_payload_hash,
                event.outcome.and_then(Outcome::is_interrupt),
            ],
        )?;
        Ok(())
    }

    /// Store `bytes` as a flushed blob and return its hash for the row.
    fn put(&self, bytes: Option<&str>) -> Result<Option<Vec<u8>>, Error> {
        match bytes {
            Some(bytes) => Ok(Some(self.blobs.put(bytes.as_bytes())?.as_bytes().to_vec())),
            None => Ok(None),
        }
    }
}

fn to_row_id(event_id: u64) -> Result<i64, Error> {
    i64::try_from(event_id).map_err(|_| Error::EventIdRange(event_id))
}

/// Whether the `events` table has the column `name` (traces from before it do not).
fn has_column(conn: &Connection, name: &str) -> Result<bool, Error> {
    let mut stmt = conn.prepare("SELECT 1 FROM pragma_table_info('events') WHERE name = ?1")?;
    Ok(stmt.exists([name])?)
}

/// Read every event in `event_id` order. A minimal reader for tests and tooling; the real query
/// layer is a later issue. Opens the database read-only: a column added after the trace was
/// written (see the crate docs) is read as `None` on every row, and nothing is written.
pub fn read_events(db_path: &Path) -> Result<Vec<Event>, Error> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut later = Vec::new();
    for (name, _) in LATER_COLUMNS {
        later.push(if has_column(&conn, name)? {
            name
        } else {
            "NULL"
        });
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT event_id, agent_id, session_id, vclock, kind, tool, path, payload_hash,
                tool_input, wall_time, {}
         FROM events ORDER BY event_id",
        later.join(", ")
    ))?;
    let mut rows = stmt.query([])?;
    let mut events = Vec::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let kind: String = row.get(4)?;
        let kind = EventKind::parse(&kind).ok_or(Error::UnknownKind { event_id: id, kind })?;
        // A row with no outcome has no interruption flag either; any other pair must be one
        // `append` writes.
        let outcome = match (row.get::<_, Option<String>>(12)?, row.get(14)?) {
            (None, None) => None,
            (text, is_interrupt) => Some(
                text.as_deref()
                    .and_then(|t| Outcome::from_columns(t, is_interrupt))
                    .ok_or(Error::UnknownOutcome {
                        event_id: id,
                        outcome: text,
                        is_interrupt,
                    })?,
            ),
        };
        events.push(Event {
            event_id: id as u64,
            slot: row.get(1)?,
            session_id: row.get(2)?,
            vclock: row.get(3)?,
            kind,
            tool: row.get(5)?,
            path: row.get(6)?,
            payload_hash: row.get(7)?,
            payload: row.get(8)?,
            input_payload_hash: row.get(13)?,
            input_payload: None,
            tool_use_id: row.get(10)?,
            before_event_id: row.get::<_, Option<i64>>(11)?.map(|b| b as u64),
            outcome,
            wall_time: row.get(9)?,
        });
    }
    Ok(events)
}

/// The exact payload bytes of `event`, a row read from the trace in `run_dir`: from its blob when
/// it has a `payload_hash`, from the inline `tool_input` column for a row written before blobs
/// (an issue #2 trace), and `None` when it has neither. The blob is rehashed on read, so a
/// missing or corrupt blob is an error, never wrong bytes. Reading creates nothing in `run_dir`.
pub fn read_payload(run_dir: &Path, event: &Event) -> Result<Option<Vec<u8>>, Error> {
    match (&event.payload_hash, &event.payload) {
        (Some(_), Some(_)) => Err(Error::TwoPayloads {
            event_id: event.event_id,
        }),
        (Some(hash), None) => read_named_blob(run_dir, event.event_id, hash).map(Some),
        (None, Some(inline)) => Ok(Some(inline.clone().into_bytes())),
        (None, None) => Ok(None),
    }
}

/// The exact raw `tool_input` bytes an AfterTool row reported, from the blob named by its
/// `input_payload_hash`; `None` for a row without one. Checked like [`read_payload`].
pub fn read_input_payload(run_dir: &Path, event: &Event) -> Result<Option<Vec<u8>>, Error> {
    event
        .input_payload_hash
        .as_ref()
        .map(|hash| read_named_blob(run_dir, event.event_id, hash))
        .transpose()
}

fn read_named_blob(run_dir: &Path, event_id: u64, hash: &[u8]) -> Result<Vec<u8>, Error> {
    let hash: [u8; 32] = hash.try_into().map_err(|_| Error::BadPayloadHash {
        event_id,
        len: hash.len(),
    })?;
    Ok(read_blob(run_dir, &BlobHash::from(hash))?)
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
            payload: None,
            input_payload_hash: None,
            input_payload: None,
            tool_use_id: Some(format!("toolu_{id}")),
            before_event_id: None,
            outcome: None,
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
        let response = "null";
        let mut full = event(2, EventKind::AfterTool);
        full.path = Some("src/lib.rs".into());
        full.payload = Some(response.into());
        full.input_payload = Some(raw.into());
        full.before_event_id = Some(1);
        full.outcome = Some(Outcome::Success);
        w.append(&full).unwrap();
        w.append(&event(1, EventKind::BeforeTool)).unwrap();

        let got = read_events(&tmp.path().join(TRACE_FILE)).unwrap();
        // The row keeps the payloads' hashes, not the payloads.
        let mut stored = full.clone();
        stored.payload = None;
        stored.input_payload = None;
        stored.payload_hash = Some(BlobHash::of(response.as_bytes()).as_bytes().to_vec());
        stored.input_payload_hash = Some(BlobHash::of(raw.as_bytes()).as_bytes().to_vec());
        assert_eq!(got, vec![event(1, EventKind::BeforeTool), stored]);
        assert_eq!(read_payload(tmp.path(), &got[0]).unwrap(), None);
        assert_eq!(read_input_payload(tmp.path(), &got[0]).unwrap(), None);
        assert_eq!(
            read_payload(tmp.path(), &got[1]).unwrap().as_deref(),
            Some(response.as_bytes())
        );
        assert_eq!(
            read_input_payload(tmp.path(), &got[1]).unwrap().as_deref(),
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
        let mut forged = event(1, EventKind::AfterTool);
        forged.input_payload_hash = Some(vec![7; 32]);
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
            EventKind::Stop,
            EventKind::SessionEnd,
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
