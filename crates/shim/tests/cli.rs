//! End-to-end tests of the `art-shim` binary against a fake coordinator on a real Unix socket.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use runtime_proto::v1::{
    admission::Verdict,
    coordinator_server::{Coordinator, CoordinatorServer},
    Admission, BeforeToolRequest,
};
use serde_json::{json, Value};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{transport::Server, Request, Response, Status};

const EDIT: &str = include_str!("fixtures/pre_tool_use_edit.json");
const BASH: &str = include_str!("fixtures/pre_tool_use_bash.json");
const POST: &str = include_str!("fixtures/post_tool_use_edit.json");

/// Run the shim with `stdin`. `env` is `(slot, socket)`; `None` leaves that variable unset. The
/// parent's `AGENT_RUNTIME_*` variables are always cleared first.
fn run_shim(slot: Option<&str>, socket: Option<&Path>, stdin: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_art-shim"));
    cmd.env_remove("AGENT_RUNTIME_SLOT")
        .env_remove("AGENT_RUNTIME_SOCKET")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(slot) = slot {
        cmd.env("AGENT_RUNTIME_SLOT", slot);
    }
    if let Some(socket) = socket {
        cmd.env("AGENT_RUNTIME_SOCKET", socket);
    }
    let mut child = cmd.spawn().expect("spawn art-shim");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for art-shim")
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).unwrap()
}

fn assert_fail_closed(out: &Output) {
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(out));
    assert_eq!(stdout(out), "");
    assert!(
        stderr(out).starts_with("agent-runtime:"),
        "stderr: {}",
        stderr(out)
    );
}

/// A new directory for sockets under `$TMPDIR`. macOS limits a Unix socket path to about 104 bytes; the default `$TMPDIR` there is
/// about 50, so the short prefix and socket names below keep the path in bounds.
fn socket_dir() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("ars")
        .tempdir()
        .expect("create a socket directory under the temp dir");
    assert!(
        dir.path().as_os_str().len() < 80,
        "socket directory path is too long for a Unix socket: {}",
        dir.path().display()
    );
    dir
}

#[derive(Clone)]
struct Fake {
    reply: Admission,
    calls: Arc<Mutex<Vec<BeforeToolRequest>>>,
}

#[tonic::async_trait]
impl Coordinator for Fake {
    async fn before_tool(
        &self,
        request: Request<BeforeToolRequest>,
    ) -> Result<Response<Admission>, Status> {
        self.calls.lock().unwrap().push(request.into_inner());
        Ok(Response::new(self.reply.clone()))
    }
}

/// A fake coordinator serving on a Unix socket in its own thread. Dropping it stops the server.
struct FakeCoordinator {
    socket: PathBuf,
    calls: Arc<Mutex<Vec<BeforeToolRequest>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    _dir: tempfile::TempDir,
}

impl FakeCoordinator {
    fn start(reply: Admission) -> Self {
        let dir = socket_dir();
        let socket = dir.path().join("c.sock");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fake = Fake {
            reply,
            calls: calls.clone(),
        };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let path = socket.clone();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = match tokio::net::UnixListener::bind(&path) {
                    Ok(l) => l,
                    Err(e) => {
                        ready_tx.send(Err(e)).unwrap();
                        return;
                    }
                };
                ready_tx.send(Ok(())).unwrap();
                Server::builder()
                    .add_service(CoordinatorServer::new(fake))
                    .serve_with_incoming_shutdown(UnixListenerStream::new(listener), async {
                        let _ = stopped.await;
                    })
                    .await
                    .unwrap();
            });
        });
        ready_rx.recv().unwrap().unwrap_or_else(|e| {
            panic!("could not bind a Unix socket at {}: {e}", socket.display())
        });
        FakeCoordinator {
            socket,
            calls,
            stop: Some(stop),
            thread: Some(thread),
            _dir: dir,
        }
    }

    fn allow() -> Self {
        Self::start(Admission {
            verdict: Verdict::Allow as i32,
            ..Default::default()
        })
    }

    fn calls(&self) -> Vec<BeforeToolRequest> {
        self.calls.lock().unwrap().clone()
    }
}

impl Drop for FakeCoordinator {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn parse(stdin: &str) -> Value {
    serde_json::from_str(stdin).unwrap()
}

#[test]
fn allow_prints_the_allow_object_and_the_coordinator_sees_the_raw_call() {
    let fake = FakeCoordinator::allow();
    let out = run_shim(Some("a"), Some(&fake.socket), EDIT);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        parse(&stdout(&out)),
        json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow"}})
    );

    let hook = parse(EDIT);
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.slot, "a");
    assert_eq!(call.session_id, hook["session_id"]);
    assert_eq!(call.tool, "Edit");
    assert_eq!(call.intent, "");
    assert_eq!(parse(&call.tool_input_json), hook["tool_input"]);
}

#[test]
fn bash_description_reaches_the_coordinator_as_intent() {
    let fake = FakeCoordinator::allow();
    let out = run_shim(Some("b"), Some(&fake.socket), BASH);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let hook = parse(BASH);
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].slot, "b");
    assert_eq!(calls[0].tool, "Bash");
    assert_eq!(calls[0].intent, hook["tool_input"]["description"]);
    assert_eq!(parse(&calls[0].tool_input_json), hook["tool_input"]);
}

#[test]
fn deny_maps_to_the_deny_object() {
    let fake = FakeCoordinator::start(Admission {
        verdict: Verdict::Deny as i32,
        reason: "b holds the lease on /work/src/lib.rs".into(),
        owner: "b".into(),
        ..Default::default()
    });
    let out = run_shim(Some("a"), Some(&fake.socket), EDIT);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        parse(&stdout(&out)),
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "b holds the lease on /work/src/lib.rs"
        }})
    );
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn allow_passes_updated_input_through() {
    let fake = FakeCoordinator::start(Admission {
        verdict: Verdict::Allow as i32,
        updated_input_json: r#"{"file_path":"/work/.art/snapshots/lib.rs"}"#.into(),
        ..Default::default()
    });
    let out = run_shim(Some("a"), Some(&fake.socket), EDIT);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        parse(&stdout(&out)),
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": {"file_path": "/work/.art/snapshots/lib.rs"}
        }})
    );
}

#[test]
fn defer_fails_closed_in_this_slice() {
    let fake = FakeCoordinator::start(Admission {
        verdict: Verdict::Defer as i32,
        ..Default::default()
    });
    assert_fail_closed(&run_shim(Some("a"), Some(&fake.socket), EDIT));
}

#[test]
fn without_slot_variables_the_shim_is_silent_and_never_calls_the_coordinator() {
    // A coordinator is listening, but with both variables unset the shim cannot know where, so the
    // only way to detect contact is the fake's call count staying at zero.
    let fake = FakeCoordinator::allow();
    let out = run_shim(None, None, EDIT);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
    assert_eq!(fake.calls().len(), 0);
}

#[test]
fn empty_variables_count_as_unset() {
    let fake = FakeCoordinator::allow();
    let out = run_shim(Some(""), Some(Path::new("")), EDIT);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(fake.calls().len(), 0);
}

#[test]
fn exactly_one_variable_set_fails_closed() {
    let fake = FakeCoordinator::allow();
    assert_fail_closed(&run_shim(Some("a"), None, EDIT));
    assert_fail_closed(&run_shim(None, Some(&fake.socket), EDIT));
    assert_eq!(fake.calls().len(), 0);
}

#[test]
fn missing_socket_fails_closed() {
    let dir = socket_dir();
    let out = run_shim(Some("a"), Some(&dir.path().join("absent.sock")), EDIT);
    assert_fail_closed(&out);
}

#[test]
fn socket_file_with_no_listener_fails_closed() {
    let dir = socket_dir();
    let path = dir.path().join("dead.sock");
    // Binding creates the socket file; dropping the listener leaves it behind with nobody
    // accepting, so connecting is refused.
    drop(std::os::unix::net::UnixListener::bind(&path).expect("bind a Unix socket"));
    assert!(path.exists());
    assert_fail_closed(&run_shim(Some("a"), Some(&path), EDIT));
}

#[test]
fn malformed_input_fails_closed_without_contacting_the_coordinator() {
    let fake = FakeCoordinator::allow();
    for bad in ["", "not json", "[]", r#"{"hook_event_name":"PreToolUse"}"#] {
        assert_fail_closed(&run_shim(Some("a"), Some(&fake.socket), bad));
    }
    assert_eq!(fake.calls().len(), 0);
}

#[test]
fn other_hook_events_are_silent_for_now() {
    // PostToolUse reporting is a later issue.
    let fake = FakeCoordinator::allow();
    let out = run_shim(Some("a"), Some(&fake.socket), POST);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(fake.calls().len(), 0);
}
