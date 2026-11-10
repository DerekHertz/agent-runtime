//! The tonic adapter: carries the pure core over gRPC on a Unix socket and appends its recorded
//! events to the run's SQLite trace (SPEC section 7).
//!
//! All decisions live in [`crate::core::step`]; this file only converts messages, performs the
//! `Record` output, and answers with the `Admit` or `Ack` output.

use std::fs::{File, TryLockError};
use std::future::Future;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use runtime_proto::v1::admission::Verdict as WireVerdict;
use runtime_proto::v1::after_tool_report::Result as WireResult;
use runtime_proto::v1::coordinator_server::{Coordinator, CoordinatorServer};
use runtime_proto::v1::{
    Ack as WireAck, Admission as WireAdmission, AfterToolReport, BeforeToolRequest,
};
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};
use trace_store::TraceWriter;

use crate::core::{
    self, Ack, Admission, AfterTool, BeforeTool, Input, Output, RunConfig, State, ToolResult,
    Verdict,
};

#[derive(Debug)]
pub struct ServeConfig {
    /// `runs/<run-id>/`: holds `trace.db`.
    pub run_dir: PathBuf,
    /// Where the shims connect.
    pub socket: PathBuf,
    pub slots: Vec<String>,
}

/// The coordinator's mutable half: the core's state and the trace it is recorded in.
struct Inner {
    state: State,
    trace: TraceWriter,
}

/// Implements the generated `Coordinator` service.
pub struct CoordinatorService {
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for CoordinatorService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoordinatorService").finish_non_exhaustive()
    }
}

impl CoordinatorService {
    pub fn new(config: &RunConfig, trace: TraceWriter) -> anyhow::Result<Self> {
        let state = State::new(config)?;
        Ok(Self {
            inner: Mutex::new(Inner { state, trace }),
        })
    }

    /// Step the core on a copy of the state, append its recorded event, and only then commit the
    /// new state; return the core's answer (`Admit` or `Ack`). If the append fails the old state
    /// stays, so a retry reuses the same event id and clock stamp, binds no session and completes
    /// no call, and the caller gets an error (the shim turns it into exit 2 for a BeforeTool and
    /// a nonblocking exit 1 for a report).
    fn step_and_record(&self, input: Input, now: i64) -> Result<Output, Status> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| Status::internal("coordinator state is poisoned"))?;
        let (next, outputs) = core::step(inner.state.clone(), input, now);
        let mut answer = None;
        for output in outputs {
            match output {
                Output::Record(event) => inner
                    .trace
                    .append(&event)
                    .map_err(|e| Status::internal(format!("trace append failed: {e}")))?,
                other => answer = Some(other),
            }
        }
        let answer =
            answer.ok_or_else(|| Status::internal("coordinator core produced no answer"))?;
        inner.state = next;
        Ok(answer)
    }

    fn before_tool(&self, call: BeforeTool, now: i64) -> Result<Admission, Status> {
        match self.step_and_record(Input::BeforeTool(call), now)? {
            Output::Admit(admission) => Ok(admission),
            _ => Err(Status::internal(
                "coordinator core answered a BeforeTool with no admission",
            )),
        }
    }

    /// A refused report recorded nothing and is a `FailedPrecondition` with the core's reason.
    fn after_tool(&self, report: AfterTool, now: i64) -> Result<(), Status> {
        match self.step_and_record(Input::AfterTool(report), now)? {
            Output::Ack(Ack::Recorded) => Ok(()),
            Output::Ack(Ack::Refused { reason }) => Err(Status::failed_precondition(reason)),
            _ => Err(Status::internal(
                "coordinator core answered an AfterTool with no ack",
            )),
        }
    }
}

#[tonic::async_trait]
impl Coordinator for CoordinatorService {
    async fn before_tool(
        &self,
        request: Request<BeforeToolRequest>,
    ) -> Result<Response<WireAdmission>, Status> {
        let req = request.into_inner();
        let call = BeforeTool {
            slot: req.slot,
            session_id: req.session_id,
            tool: req.tool,
            tool_input_json: req.tool_input_json,
            intent: req.intent,
            tool_use_id: req.tool_use_id,
        };
        let admission = self.before_tool(call, now_ms())?;
        Ok(Response::new(to_wire(admission)))
    }

    async fn after_tool(
        &self,
        request: Request<AfterToolReport>,
    ) -> Result<Response<WireAck>, Status> {
        let req = request.into_inner();
        // An unset oneof is a report without a result; it is refused, never taken as success.
        let result = match req.result {
            Some(WireResult::Success(success)) => ToolResult::Success {
                tool_response_json: success.tool_response_json,
            },
            // An unset is_interrupt stays None: the hook omitted it, which is not `false`.
            Some(WireResult::Failure(failure)) => ToolResult::Failure {
                error_json: failure.error_json,
                is_interrupt: failure.is_interrupt,
            },
            None => {
                return Err(Status::invalid_argument(
                    "the AfterTool report has no result",
                ))
            }
        };
        let report = AfterTool {
            slot: req.slot,
            session_id: req.session_id,
            tool: req.tool,
            tool_input_json: req.tool_input_json,
            tool_use_id: req.tool_use_id,
            result,
        };
        self.after_tool(report, now_ms())?;
        Ok(Response::new(WireAck {}))
    }
}

fn to_wire(a: Admission) -> WireAdmission {
    let verdict = match a.verdict {
        Verdict::Allow => WireVerdict::Allow,
        Verdict::Deny => WireVerdict::Deny,
        Verdict::Defer => WireVerdict::Defer,
    };
    WireAdmission {
        verdict: verdict as i32,
        reason: a.reason,
        owner: a.owner,
        updated_input_json: a.updated_input_json,
    }
}

/// Wall time in milliseconds since the Unix epoch. Display only, never for ordering.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Removes the socket file when dropped, so it goes away on every exit path. It only removes the
/// file it bound (same device and inode), never a socket another coordinator put at that path.
struct SocketGuard {
    path: PathBuf,
    id: (u64, u64),
}

fn file_id(meta: &std::fs::Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if let Ok(meta) = std::fs::symlink_metadata(&self.path) {
            if file_id(&meta) == self.id {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

/// A coordinator whose trace is open and whose socket is bound but not yet serving. Connections
/// queue in the socket's backlog until [`Listening::serve`] runs.
pub struct Listening {
    listener: UnixListener,
    service: CoordinatorService,
    socket: SocketGuard,
    /// Exclusive lock on `<socket>.lock`, declared after `socket` so the socket file is removed
    /// before another coordinator can take the path.
    _socket_lock: File,
}

/// Take the socket path for this coordinator: an exclusive lock on `<socket>.lock`, held for the
/// coordinator's lifetime. Every start takes it before probing, removing or binding the socket,
/// so two starts on one path cannot both find the same stale socket.
fn lock_socket_path(socket: &Path) -> anyhow::Result<File> {
    let mut name = socket.as_os_str().to_owned();
    name.push(".lock");
    let lock_path = PathBuf::from(name);
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening {}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => {
            anyhow::bail!("{} is in use by a running coordinator", socket.display())
        }
        Err(TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("locking {}", lock_path.display()))
        }
    }
}

impl std::fmt::Debug for Listening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listening").finish_non_exhaustive()
    }
}

/// Create the trace, remove a stale socket file, and bind the socket. The socket file exists
/// when this returns.
pub fn bind(config: ServeConfig) -> anyhow::Result<Listening> {
    let run = RunConfig {
        slots: config.slots,
    };
    run.validate()?;
    if let Some(parent) = config.socket.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    // Own the socket path, then refuse a live coordinator's socket, before touching the trace.
    let socket_lock = lock_socket_path(&config.socket)?;
    remove_stale_socket(&config.socket)?;
    let trace = TraceWriter::create(&config.run_dir)
        .with_context(|| format!("creating trace in {}", config.run_dir.display()))?;
    let service = CoordinatorService::new(&run, trace)?;

    let listener = UnixListener::bind(&config.socket)
        .with_context(|| format!("binding {}", config.socket.display()))?;
    let meta = std::fs::symlink_metadata(&config.socket)
        .with_context(|| format!("checking {}", config.socket.display()))?;
    Ok(Listening {
        listener,
        service,
        socket: SocketGuard {
            path: config.socket,
            id: file_id(&meta),
        },
        _socket_lock: socket_lock,
    })
}

/// Only a leftover socket is removed; anything else at that path is an error, not ours to delete.
/// A socket that still accepts connections belongs to a live coordinator and is refused.
fn remove_stale_socket(path: &Path) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                anyhow::bail!("{} is in use by a running coordinator", path.display());
            }
            std::fs::remove_file(path)
                .with_context(|| format!("removing stale socket {}", path.display()))
        }
        Ok(_) => anyhow::bail!("{} exists and is not a socket", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("checking {}", path.display())),
    }
}

impl Listening {
    /// Serve until `shutdown` resolves, then remove the socket file.
    pub async fn serve(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> anyhow::Result<()> {
        // Locals drop in reverse order of binding: the guard removes the socket file first, then
        // the path lock is released.
        let Listening {
            _socket_lock,
            socket: _socket,
            listener,
            service,
        } = self;
        tonic::transport::Server::builder()
            .add_service(CoordinatorServer::new(service))
            .serve_with_incoming_shutdown(UnixListenerStream::new(listener), shutdown)
            .await
            .context("serving the coordinator")
    }
}

pub async fn serve(
    config: ServeConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    bind(config)?.serve(shutdown).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::identity::CallKey;
    use trace_store::read_events;

    fn call(slot: &str, session_id: &str, tool_use_id: &str) -> BeforeTool {
        BeforeTool {
            slot: slot.into(),
            session_id: session_id.into(),
            tool: "Edit".into(),
            tool_input_json: "{}".into(),
            intent: String::new(),
            tool_use_id: tool_use_id.into(),
        }
    }

    fn service(dir: &Path, slots: &[&str]) -> CoordinatorService {
        let config = RunConfig {
            slots: slots.iter().map(|s| s.to_string()).collect(),
        };
        CoordinatorService::new(&config, TraceWriter::create(dir).unwrap()).unwrap()
    }

    #[test]
    fn failed_append_keeps_the_old_state() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service(tmp.path(), &["slot-1", "slot-2"]);
        // Plant event 2 in the trace behind the writer's back (a second TraceWriter is refused
        // while `svc` holds the run), so the second call's append collides with it.
        rusqlite::Connection::open(tmp.path().join("trace.db"))
            .unwrap()
            .execute(
                "INSERT INTO events (event_id, agent_id, session_id, vclock, kind, wall_time)
                 VALUES (2, 'planted', 'x', x'', 'BeforeTool', 0)",
                [],
            )
            .unwrap();

        assert_eq!(
            svc.before_tool(call("slot-1", "s", "t1"), 5)
                .unwrap()
                .verdict,
            Verdict::Allow
        );
        // A new session's first call fails to append: it must bind nothing and use no identity.
        let err = svc.before_tool(call("slot-2", "s2", "t2"), 6).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Internal);

        let inner = svc.inner.lock().unwrap();
        assert_eq!(inner.state.next_event_id(), 2);
        assert_eq!(inner.state.clock("slot-1").unwrap().entries(), &[1, 0]);
        assert_eq!(inner.state.clock("slot-2").unwrap().entries(), &[0, 0]);
        assert_eq!(inner.state.session_slot("s"), Some("slot-1"));
        assert_eq!(inner.state.session_slot("s2"), None);
        let failed = CallKey {
            session_id: "s2".into(),
            tool_use_id: "t2".into(),
        };
        assert_eq!(inner.state.call(&failed), None);
        drop(inner);
        // Only the planted row and the first call's row are in the trace.
        let ids: Vec<u64> = read_events(&tmp.path().join("trace.db"))
            .unwrap()
            .iter()
            .map(|e| e.event_id)
            .collect();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn failed_after_tool_append_completes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service(tmp.path(), &["slot-1", "slot-2"]);
        svc.before_tool(call("slot-1", "s", "t1"), 1).unwrap();
        let db = rusqlite::Connection::open(tmp.path().join("trace.db")).unwrap();
        db.execute(
            "INSERT INTO events (event_id, agent_id, session_id, vclock, kind, wall_time)
             VALUES (2, 'planted', 'x', x'', 'BeforeTool', 0)",
            [],
        )
        .unwrap();
        let report = || AfterTool {
            slot: "slot-1".into(),
            session_id: "s".into(),
            tool: "Edit".into(),
            tool_input_json: "{}".into(),
            tool_use_id: "t1".into(),
            result: ToolResult::Success {
                tool_response_json: "null".into(),
            },
        };
        let key = CallKey {
            session_id: "s".into(),
            tool_use_id: "t1".into(),
        };
        let err = svc.after_tool(report(), 2).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Internal);
        {
            let inner = svc.inner.lock().unwrap();
            assert_eq!(inner.state.next_event_id(), 2);
            assert_eq!(inner.state.clock("slot-1").unwrap().entries(), &[1, 0]);
            assert_eq!(inner.state.call(&key).unwrap().completed_event_id, None);
        }
        db.execute_batch("DROP TRIGGER events_no_delete; DELETE FROM events WHERE event_id = 2;")
            .unwrap();
        svc.after_tool(report(), 3).unwrap();
        svc.after_tool(report(), 4).unwrap();
        let inner = svc.inner.lock().unwrap();
        let recorded = inner.state.call(&key).unwrap();
        assert_eq!(
            (recorded.before_event_id, recorded.completed_event_id),
            (Some(1), Some(2)),
            "the retry is the first completion and the repeat does not replace it"
        );
        assert_eq!(inner.state.next_event_id(), 4);
    }

    #[test]
    fn failed_failure_report_append_completes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service(tmp.path(), &["slot-1", "slot-2"]);
        svc.before_tool(call("slot-1", "s", "t1"), 1).unwrap();
        let db = rusqlite::Connection::open(tmp.path().join("trace.db")).unwrap();
        db.execute(
            "INSERT INTO events (event_id, agent_id, session_id, vclock, kind, wall_time)
             VALUES (2, 'planted', 'x', x'', 'BeforeTool', 0)",
            [],
        )
        .unwrap();
        let report = |result| AfterTool {
            slot: "slot-1".into(),
            session_id: "s".into(),
            tool: "Edit".into(),
            tool_input_json: "{}".into(),
            tool_use_id: "t1".into(),
            result,
        };
        let failure = || ToolResult::Failure {
            error_json: r#""interrupted""#.into(),
            is_interrupt: Some(true),
        };
        let key = CallKey {
            session_id: "s".into(),
            tool_use_id: "t1".into(),
        };
        let err = svc.after_tool(report(failure()), 2).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Internal);
        {
            let inner = svc.inner.lock().unwrap();
            assert_eq!(inner.state.next_event_id(), 2);
            assert_eq!(inner.state.clock("slot-1").unwrap().entries(), &[1, 0]);
            assert_eq!(inner.state.call(&key).unwrap().completed_event_id, None);
        }
        db.execute_batch("DROP TRIGGER events_no_delete; DELETE FROM events WHERE event_id = 2;")
            .unwrap();
        svc.after_tool(report(failure()), 3).unwrap();
        let success = ToolResult::Success {
            tool_response_json: "{}".into(),
        };
        svc.after_tool(report(success), 4).unwrap();
        let inner = svc.inner.lock().unwrap();
        let recorded = inner.state.call(&key).unwrap();
        assert_eq!(
            (recorded.before_event_id, recorded.completed_event_id),
            (Some(1), Some(2)),
            "the retried failure is the first completion; a later success does not replace it"
        );
        assert_eq!(inner.state.next_event_id(), 4);
    }

    #[test]
    fn unknown_slot_is_denied_without_recording() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service(tmp.path(), &["slot-1"]);
        let adm = svc.before_tool(call("nope", "s", "t1"), 0).unwrap();
        assert_eq!(adm.verdict, Verdict::Deny);
        assert!(read_events(&tmp.path().join("trace.db"))
            .unwrap()
            .is_empty());
    }

    fn config(dir: &Path, run: &str) -> ServeConfig {
        ServeConfig {
            run_dir: dir.join(run),
            socket: dir.join("c.sock"),
            slots: vec!["slot-1".into()],
        }
    }

    #[tokio::test]
    async fn a_second_coordinator_cannot_take_a_live_socket() {
        let tmp = tempfile::Builder::new().prefix("art").tempdir().unwrap();
        let first = bind(config(tmp.path(), "run-a")).unwrap();
        let id = file_id(&std::fs::symlink_metadata(tmp.path().join("c.sock")).unwrap());

        let err = bind(config(tmp.path(), "run-b")).expect_err("second bind refused");
        assert!(err.to_string().contains("in use"), "{err:#}");
        // The refused coordinator touched neither the first one's socket nor a trace of its own.
        let meta = std::fs::symlink_metadata(tmp.path().join("c.sock")).unwrap();
        assert_eq!(file_id(&meta), id);
        assert!(!tmp.path().join("run-b").exists());
        drop(first);
        assert!(!tmp.path().join("c.sock").exists());
    }

    #[tokio::test]
    async fn a_stale_socket_is_replaced_and_the_old_guard_leaves_the_new_one() {
        let tmp = tempfile::Builder::new().prefix("art").tempdir().unwrap();
        let socket = tmp.path().join("c.sock");
        // A guard for a socket file that has since been replaced by another one.
        let stale = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let old_guard = SocketGuard {
            path: socket.clone(),
            id: file_id(&std::fs::symlink_metadata(&socket).unwrap()),
        };
        drop(stale); // nobody listens now: the file is stale
        let second = bind(config(tmp.path(), "run-b")).unwrap();
        drop(old_guard);
        assert!(socket.exists(), "old guard must not delete the new socket");
        drop(second);
        assert!(!socket.exists());
    }

    #[test]
    fn simultaneous_starts_on_one_stale_socket_leave_exactly_one_live_coordinator() {
        const STARTS: usize = 8;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for round in 0..20 {
            let tmp = tempfile::Builder::new().prefix("art").tempdir().unwrap();
            let socket = tmp.path().join("c.sock");
            // A stale socket: bound once, nobody listening any more.
            drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());

            let barrier = std::sync::Arc::new(std::sync::Barrier::new(STARTS));
            let handles: Vec<_> = (0..STARTS)
                .map(|i| {
                    let (barrier, handle) = (barrier.clone(), runtime.handle().clone());
                    let dir = tmp.path().to_path_buf();
                    std::thread::spawn(move || {
                        let _entered = handle.enter();
                        barrier.wait();
                        bind(config(&dir, &format!("run-{i}")))
                    })
                })
                .collect();
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

            let (won, lost): (Vec<_>, Vec<_>) = results.into_iter().partition(|r| r.is_ok());
            assert_eq!(won.len(), 1, "round {round}: exactly one start wins");
            for err in lost {
                let err = err.expect_err("partitioned as lost");
                assert!(err.to_string().contains("in use"), "round {round}: {err:#}");
            }
            let winner = won.into_iter().next().unwrap().unwrap();
            // The live socket at the path is the winner's own, and it accepts connections.
            let meta = std::fs::symlink_metadata(&socket).unwrap();
            assert_eq!(file_id(&meta), winner.socket.id, "round {round}");
            std::os::unix::net::UnixStream::connect(&socket).expect("winner reachable");
            let runs = (0..STARTS)
                .filter(|i| tmp.path().join(format!("run-{i}")).exists())
                .count();
            assert_eq!(runs, 1, "round {round}: losers never touch a run");
            drop(winner);
            assert!(!socket.exists());
        }
    }

    #[test]
    fn bind_refuses_a_run_that_already_has_events() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        let svc = service(&run_dir, &["slot-1"]);
        svc.before_tool(call("slot-1", "s", "t1"), 0).unwrap();
        drop(svc);
        let err = bind(config(tmp.path(), "run")).expect_err("nonempty run refused");
        assert!(
            format!("{err:#}").contains("resuming a run is not supported"),
            "{err:#}"
        );
    }

    #[test]
    fn bind_refuses_a_non_socket_at_the_socket_path() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("c.sock");
        std::fs::write(&socket, b"not a socket").unwrap();
        let result = bind(ServeConfig {
            run_dir: tmp.path().join("run"),
            socket,
            slots: vec!["slot-1".into()],
        });
        assert!(result.is_err());
    }
}
