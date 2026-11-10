//! A finished run loaded into memory, and the causal queries over it (SPEC section 5, "Queries
//! the store must answer").
//!
//! SQLite and the blob store are read once, in [`Run::load`]. Every query after that is a pure
//! method on the loaded [`Run`]: no SQL and no file access. Causal order comes only from the
//! events' vector clocks; `event_id` is storage order and `wall_time` is for display.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use clock::{Clock, Order};

use crate::{read_events, read_payload, Access, Body, Event, Outcome, ShapeError, TRACE_FILE};

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Trace(#[from] crate::Error),
    #[error(transparent)]
    Shape(#[from] ShapeError),
    #[error("event {event_id} has a clock that cannot be decoded: {source}")]
    Clock {
        event_id: u64,
        source: clock::DecodeError,
    },
    #[error("event {0} appears twice in the run")]
    DuplicateEvent(u64),
    #[error("event {0} is not in the run")]
    NoSuchEvent(u64),
    #[error("event {event_id} belongs to slot {actual:?}, not {expected:?}")]
    OtherSlot {
        event_id: u64,
        expected: String,
        actual: String,
    },
}

/// One event of a loaded run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEvent {
    pub event: Event,
    pub clock: Clock,
    /// The file this event writes, when it is a write (see [`Run::writes`]).
    pub write_path: Option<String>,
}

/// Two writes to one path by different slots, neither of which happens-before the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcurrentWrites {
    pub path: String,
    /// The earlier event id of the pair.
    pub first: u64,
    pub second: u64,
}

/// A run's events in `event_id` order, indexed by id.
#[derive(Debug, Clone, Default)]
pub struct Run {
    events: Vec<RunEvent>,
    index: HashMap<u64, usize>,
}

impl Run {
    /// Load `run_dir/trace.db` and the input of every write call. Reads only.
    pub fn load(run_dir: &Path) -> Result<Run, RunError> {
        let rows = read_events(&run_dir.join(TRACE_FILE))?;
        let mut inputs = HashMap::new();
        for row in &rows {
            if needs_input(row)? {
                if let Some(bytes) = read_payload(run_dir, row)? {
                    inputs.insert(row.event_id, bytes);
                }
            }
        }
        Run::from_events(rows, |event| inputs.remove(&event.event_id))
    }

    /// Build a run from rows already in memory. `input_of` returns the raw tool input of a
    /// BeforeTool, and is asked only for a write tool's row that has no recorded path (a trace
    /// written before the coordinator classified calls).
    pub fn from_events(
        rows: Vec<Event>,
        mut input_of: impl FnMut(&Event) -> Option<Vec<u8>>,
    ) -> Result<Run, RunError> {
        let failed = failed_calls(&rows)?;
        let mut run = Run::default();
        for event in rows {
            let clock = Clock::from_bytes(&event.vclock).map_err(|source| RunError::Clock {
                event_id: event.event_id,
                source,
            })?;
            let write_path = match event.body()? {
                Body::BeforeTool { tool, path, .. }
                    if Access::is_write_tool(tool) && !failed.contains(&event.event_id) =>
                {
                    match path {
                        Some(path) => Some(path.to_owned()),
                        None => input_of(&event).and_then(|input| {
                            Access::classify(tool, &input).path().map(str::to_owned)
                        }),
                    }
                }
                _ => None,
            };
            if run.index.insert(event.event_id, run.events.len()).is_some() {
                return Err(RunError::DuplicateEvent(event.event_id));
            }
            run.events.push(RunEvent {
                event,
                clock,
                write_path,
            });
        }
        Ok(run)
    }

    pub fn events(&self) -> &[RunEvent] {
        &self.events
    }

    pub fn get(&self, event_id: u64) -> Result<&RunEvent, RunError> {
        self.index
            .get(&event_id)
            .map(|&i| &self.events[i])
            .ok_or(RunError::NoSuchEvent(event_id))
    }

    /// Whether `a` happens-before `b`: every entry of `a`'s clock is at most `b`'s, and at least
    /// one is smaller. A strict partial order.
    pub fn happens_before(&self, a: u64, b: u64) -> Result<bool, RunError> {
        Ok(self.get(a)?.clock.compare(&self.get(b)?.clock) == Order::Before)
    }

    /// Whether `a` and `b` are two events neither of which happens-before the other. Symmetric,
    /// and never true of an event and itself.
    pub fn concurrent(&self, a: u64, b: u64) -> Result<bool, RunError> {
        let (ea, eb) = (self.get(a)?, self.get(b)?);
        Ok(a != b && concurrent(ea, eb))
    }

    /// Whether `slot` knew of `event` when it recorded its own event `at`: `at`'s clock is at
    /// least `event`'s in every entry.
    pub fn knew(&self, slot: &str, event: u64, at: u64) -> Result<bool, RunError> {
        let (known, when) = (self.get(event)?, self.get(at)?);
        if when.event.slot != slot {
            return Err(RunError::OtherSlot {
                event_id: at,
                expected: slot.to_owned(),
                actual: when.event.slot.clone(),
            });
        }
        Ok(matches!(
            known.clock.compare(&when.clock),
            Order::Before | Order::Equal
        ))
    }

    /// Every event that happens-before `event`, in `event_id` order.
    pub fn causal_past(&self, event: u64) -> Result<Vec<u64>, RunError> {
        let target = &self.get(event)?.clock;
        Ok(self
            .events
            .iter()
            .filter(|e| e.clock.compare(target) == Order::Before)
            .map(|e| e.event.event_id)
            .collect())
    }

    /// The run's writes: each BeforeTool of Edit, Write or NotebookEdit whose input names a file
    /// and whose call did not complete as a failure. The BeforeTool is the write's event, because
    /// its clock is what the slot knew when it decided to write.
    pub fn writes(&self) -> impl Iterator<Item = &RunEvent> {
        self.events.iter().filter(|e| e.write_path.is_some())
    }

    /// Every pair of concurrent writes to one path by two slots, ordered by path, then by the
    /// pair's event ids. The coherence layer exists to make this list empty.
    pub fn concurrent_writes(&self) -> Vec<ConcurrentWrites> {
        let mut by_path: BTreeMap<&str, Vec<&RunEvent>> = BTreeMap::new();
        for write in self.writes() {
            let path = write.write_path.as_deref().expect("writes() keeps paths");
            by_path.entry(path).or_default().push(write);
        }
        let mut pairs = Vec::new();
        for (path, writes) in by_path {
            for (i, first) in writes.iter().enumerate() {
                for second in &writes[i + 1..] {
                    if first.event.slot != second.event.slot && concurrent(first, second) {
                        pairs.push(ConcurrentWrites {
                            path: path.to_owned(),
                            first: first.event.event_id,
                            second: second.event.event_id,
                        });
                    }
                }
            }
        }
        pairs
    }
}

fn concurrent(a: &RunEvent, b: &RunEvent) -> bool {
    matches!(a.clock.compare(&b.clock), Order::Concurrent | Order::Equal)
}

/// Whether `event` is a write tool's BeforeTool with no recorded path, whose stored input must
/// be read to find the file.
fn needs_input(event: &Event) -> Result<bool, ShapeError> {
    Ok(matches!(
        event.body()?,
        Body::BeforeTool { tool, path: None, .. } if Access::is_write_tool(tool)
    ))
}

/// The BeforeTool ids of the calls whose completion (their first AfterTool) is a failure.
fn failed_calls(rows: &[Event]) -> Result<HashSet<u64>, ShapeError> {
    let mut first_outcome: HashMap<u64, Outcome> = HashMap::new();
    for row in rows {
        if let Body::AfterTool {
            before_event_id: Some(before),
            outcome,
            ..
        } = row.body()?
        {
            first_outcome.entry(before).or_insert(outcome);
        }
    }
    Ok(first_outcome
        .into_iter()
        .filter(|(_, outcome)| matches!(outcome, Outcome::Failure { .. }))
        .map(|(before, _)| before)
        .collect())
}
