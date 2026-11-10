//! End-to-end tests of the `art-shim` binary against a fake coordinator on a real Unix socket.

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use runtime_proto::v1::{
    admission::Verdict,
    after_tool_report::Result as ReportResult,
    coordinator_server::{Coordinator, CoordinatorServer},
    Ack, Admission, AfterToolReport, BeforeToolRequest,
};
use serde_json::{json, Value};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{transport::Server, Request, Response, Status};

const EDIT: &str = include_str!("fixtures/pre_tool_use_edit.json");
const BASH: &str = include_str!("fixtures/pre_tool_use_bash.json");
const POST: &str = include_str!("fixtures/post_tool_use_edit.json");
const FAILURE: &str = include_str!("fixtures/post_tool_use_failure_bash.json");

/// Run the shim with `stdin`. `env` is `(slot, socket)`; `None` leaves that variable unset. The
/// parent's `AGENT_RUNTIME_*` variables are always cleared first.
fn run_shim(slot: Option<&str>, socket: Option<&Path>, stdin: &str) -> Output {
    run_shim_as(&[], slot, socket, stdin)
}

/// The PostToolUse registration's command line.
const REPORT: &[&str] = &["--hook-event", "PostToolUse"];

/// The PostToolUseFailure registration's command line.
const FAILED: &[&str] = &["--hook-event", "PostToolUseFailure"];

/// [`run_shim`] with the registration's arguments, such as [`REPORT`].
fn run_shim_as(args: &[&str], slot: Option<&str>, socket: Option<&Path>, stdin: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_art-shim"));
    cmd.args(args)
        .env_remove("AGENT_RUNTIME_SLOT")
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
    // The shim exits before reading stdin when it is not in a run or its configuration is bad,
    // so the write can race its exit. A broken pipe is that race; the caller still checks the
    // exit code and output.
    match child.stdin.take().unwrap().write_all(stdin.as_bytes()) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
        Err(e) => panic!("write stdin: {e}"),
    }
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

/// A report that could not be made: nonblocking exit 1, nothing on stdout, a reason on stderr.
fn assert_report_failed(out: &Output) {
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(out));
    assert_eq!(stdout(out), "");
    assert!(
        stderr(out).starts_with("agent-runtime:"),
        "stderr: {}",
        stderr(out)
    );
}

fn assert_silent(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(out));
    assert_eq!(stdout(out), "");
    assert_eq!(stderr(out), "");
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
    /// `None` acks every report; `Some` refuses it with this status, after a delay.
    report_error: Option<(tonic::Code, Duration)>,
    reports: Arc<Mutex<Vec<AfterToolReport>>>,
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

    async fn after_tool(&self, request: Request<AfterToolReport>) -> Result<Response<Ack>, Status> {
        self.reports.lock().unwrap().push(request.into_inner());
        match self.report_error {
            None => Ok(Response::new(Ack {})),
            Some((code, delay)) => {
                tokio::time::sleep(delay).await;
                Err(Status::new(code, "refused by the fake coordinator"))
            }
        }
    }
}

/// A fake coordinator serving on a Unix socket in its own thread. Dropping it stops the server.
struct FakeCoordinator {
    socket: PathBuf,
    calls: Arc<Mutex<Vec<BeforeToolRequest>>>,
    reports: Arc<Mutex<Vec<AfterToolReport>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    _dir: tempfile::TempDir,
}

impl FakeCoordinator {
    fn start(reply: Admission) -> Self {
        Self::start_with(reply, None)
    }

    fn start_with(reply: Admission, report_error: Option<(tonic::Code, Duration)>) -> Self {
        let dir = socket_dir();
        let socket = dir.path().join("c.sock");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let reports = Arc::new(Mutex::new(Vec::new()));
        let fake = Fake {
            reply,
            calls: calls.clone(),
            report_error,
            reports: reports.clone(),
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
            reports,
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

    fn reports(&self) -> Vec<AfterToolReport> {
        self.reports.lock().unwrap().clone()
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
    assert_eq!(call.tool_use_id, hook["tool_use_id"]);
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
fn the_legacy_registration_lets_other_events_pass_silently() {
    // Without --hook-event the shim is the issue #2 PreToolUse gate only.
    let fake = FakeCoordinator::allow();
    assert_silent(&run_shim(Some("a"), Some(&fake.socket), POST));
    assert_silent(&run_shim(Some("a"), Some(&fake.socket), FAILURE));
    assert_eq!(fake.calls().len(), 0);
    assert_eq!(fake.reports().len(), 0);
}

#[test]
fn a_registered_pre_tool_use_gate_behaves_like_the_legacy_one() {
    let fake = FakeCoordinator::allow();
    let args = &["--hook-event", "PreToolUse"];
    let out = run_shim_as(args, Some("a"), Some(&fake.socket), EDIT);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        parse(&stdout(&out))["hookSpecificOutput"]["permissionDecision"],
        "allow"
    );
    // A gate registered for PreToolUse fails closed on any other event.
    assert_fail_closed(&run_shim_as(args, Some("a"), Some(&fake.socket), POST));
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn a_post_tool_use_report_is_silent_and_carries_the_raw_input_and_response() {
    let fake = FakeCoordinator::allow();
    let null_response = POST.replace(
        r#""tool_response":{"filePath":"/work/src/lib.rs"}"#,
        r#""tool_response":null"#,
    );
    assert_ne!(null_response, POST);
    for stdin in [POST, null_response.as_str()] {
        assert_silent(&run_shim_as(REPORT, Some("a"), Some(&fake.socket), stdin));
    }
    let reports = fake.reports();
    assert_eq!(reports.len(), 2);
    assert_eq!(fake.calls().len(), 0, "a report asks for no admission");
    let hook = parse(POST);
    let responses = [r#"{"filePath":"/work/src/lib.rs"}"#, "null"];
    for (report, response) in reports.iter().zip(responses) {
        assert_eq!(report.slot, "a");
        assert_eq!(report.session_id, hook["session_id"]);
        assert_eq!(report.tool, "Edit");
        assert_eq!(report.tool_use_id, hook["tool_use_id"]);
        assert_eq!(
            report.tool_input_json,
            r#"{"file_path":"/work/src/lib.rs","old_string":"a","new_string":"b","replace_all":false}"#
        );
        match &report.result {
            Some(ReportResult::Success(s)) => assert_eq!(s.tool_response_json, response),
            other => panic!("expected a success, got {other:?}"),
        }
    }
}

/// The failure fixture with `"error":...,"is_interrupt":false` swapped by text for `fields`, so
/// every other byte stays as recorded.
fn failure_with(fields: &str) -> String {
    let start = FAILURE.find(r#""error":"#).unwrap();
    let end = FAILURE.find(r#","duration_ms""#).unwrap();
    format!("{}{}{}", &FAILURE[..start], fields, &FAILURE[end..])
}

#[test]
fn a_post_tool_use_failure_report_is_silent_and_carries_the_raw_input_error_and_flag() {
    let fake = FakeCoordinator::allow();
    let omitted = failure_with("\"error\":\"\"");
    let interrupted = failure_with(r#""error":"stopped","is_interrupt":true"#);
    for stdin in [FAILURE, omitted.as_str(), interrupted.as_str()] {
        assert_silent(&run_shim_as(FAILED, Some("a"), Some(&fake.socket), stdin));
    }
    assert_eq!(fake.calls().len(), 0, "a report asks for no admission");
    let reports = fake.reports();
    assert_eq!(reports.len(), 3);
    let hook = parse(FAILURE);
    let fixture_error =
        &FAILURE[FAILURE.find(r#""Exit"#).unwrap()..FAILURE.find(r#","is_interrupt""#).unwrap()];
    let want = [
        (fixture_error, Some(false)),
        ("\"\"", None),
        (r#""stopped""#, Some(true)),
    ];
    for (report, (error, flag)) in reports.iter().zip(want) {
        assert_eq!(report.slot, "a");
        assert_eq!(report.session_id, hook["session_id"]);
        assert_eq!(report.tool, "Bash");
        assert_eq!(report.tool_use_id, hook["tool_use_id"]);
        assert!(FAILURE.contains(&format!(r#""tool_input":{}"#, report.tool_input_json)));
        match &report.result {
            Some(ReportResult::Failure(f)) => {
                assert_eq!((f.error_json.as_str(), f.is_interrupt), (error, flag));
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }
}

#[test]
fn a_refused_or_unreachable_failure_report_exits_1_not_2() {
    let fake = FakeCoordinator::start_with(
        Admission::default(),
        Some((tonic::Code::FailedPrecondition, Duration::ZERO)),
    );
    let out = run_shim_as(FAILED, Some("a"), Some(&fake.socket), FAILURE);
    assert_report_failed(&out);
    assert!(
        stderr(&out).contains("refused by the fake coordinator"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fake.reports().len(), 1);
    let dir = socket_dir();
    let dead = dir.path().join("dead.sock");
    drop(std::os::unix::net::UnixListener::bind(&dead).expect("bind a Unix socket"));
    for (slot, socket) in [
        (Some("a"), Some(dir.path().join("absent.sock"))),
        (Some("a"), Some(dead)),
        (Some("a"), None),
        (None, Some(fake.socket.clone())),
    ] {
        assert_report_failed(&run_shim_as(FAILED, slot, socket.as_deref(), FAILURE));
    }
    assert_eq!(fake.reports().len(), 1);
}

#[test]
fn a_malformed_mistyped_or_mismatched_failure_exits_1_without_contacting_the_coordinator() {
    let fake = FakeCoordinator::allow();
    let no_error = failure_with(r#""is_interrupt":false"#);
    let mut bad = vec![
        "".to_owned(),
        "not json".into(),
        POST.into(),
        EDIT.into(),
        no_error,
    ];
    for fields in [
        r#""error":null"#,
        r#""error":1"#,
        r#""error":{"message":"x"}"#,
        r#""error":"x","is_interrupt":null"#,
        r#""error":"x","is_interrupt":"false""#,
    ] {
        bad.push(failure_with(fields));
    }
    for stdin in &bad {
        assert_report_failed(&run_shim_as(FAILED, Some("a"), Some(&fake.socket), stdin));
    }
    // The success registration refuses a failure event the same way.
    assert_report_failed(&run_shim_as(REPORT, Some("a"), Some(&fake.socket), FAILURE));
    assert_eq!(fake.reports().len() + fake.calls().len(), 0);
}

#[test]
fn a_refused_report_fails_without_blocking() {
    let fake = FakeCoordinator::start_with(
        Admission::default(),
        Some((tonic::Code::FailedPrecondition, Duration::ZERO)),
    );
    let out = run_shim_as(REPORT, Some("a"), Some(&fake.socket), POST);
    assert_report_failed(&out);
    assert!(
        stderr(&out).contains("refused by the fake coordinator"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fake.reports().len(), 1);
}

#[test]
fn report_transport_and_config_failures_exit_1_not_2() {
    let dir = socket_dir();
    let missing = dir.path().join("absent.sock");
    assert_report_failed(&run_shim_as(REPORT, Some("a"), Some(&missing), POST));
    let dead = dir.path().join("dead.sock");
    drop(std::os::unix::net::UnixListener::bind(&dead).expect("bind a Unix socket"));
    assert_report_failed(&run_shim_as(REPORT, Some("a"), Some(&dead), POST));
    let fake = FakeCoordinator::allow();
    assert_report_failed(&run_shim_as(REPORT, Some("a"), None, POST));
    assert_report_failed(&run_shim_as(REPORT, None, Some(&fake.socket), POST));
    assert_eq!(fake.reports().len(), 0);
}

#[test]
fn malformed_or_mismatched_report_input_exits_1_without_contacting_the_coordinator() {
    let fake = FakeCoordinator::allow();
    let no_response = POST.replace(r#""tool_response":{"filePath":"/work/src/lib.rs"},"#, "");
    assert_ne!(no_response, POST);
    for bad in ["", "not json", "[]", EDIT, no_response.as_str()] {
        assert_report_failed(&run_shim_as(REPORT, Some("a"), Some(&fake.socket), bad));
    }
    assert_eq!(fake.reports().len(), 0);
    assert_eq!(fake.calls().len(), 0);
}

#[test]
fn a_hung_coordinator_ends_the_report_within_its_bound() {
    let fake = FakeCoordinator::start_with(
        Admission::default(),
        Some((tonic::Code::Unavailable, Duration::from_secs(60))),
    );
    let started = Instant::now();
    let out = run_shim_as(REPORT, Some("a"), Some(&fake.socket), POST);
    let took = started.elapsed();
    assert_report_failed(&out);
    assert!(
        stderr(&out).contains("did not answer within"),
        "{}",
        stderr(&out)
    );
    // The bound is 10 s; the documented hook timeout is 30 s.
    assert!(took < Duration::from_secs(20), "{took:?}");
}

#[test]
fn outside_a_run_a_report_is_silent_and_never_calls_the_coordinator() {
    let fake = FakeCoordinator::allow();
    assert_silent(&run_shim_as(REPORT, None, None, POST));
    assert_silent(&run_shim_as(REPORT, None, None, "not json"));
    assert_silent(&run_shim_as(FAILED, None, None, FAILURE));
    assert_silent(&run_shim_as(FAILED, None, None, "not json"));
    // Even a command line this build cannot honour stays silent outside a run.
    assert_silent(&run_shim_as(&["--hook-event", "Bogus"], None, None, POST));
    assert_eq!(fake.reports().len() + fake.calls().len(), 0);
}

#[test]
fn a_bad_command_line_in_a_run_fails_closed_unless_it_names_a_report() {
    let fake = FakeCoordinator::allow();
    for args in [
        &["--hook-event", "Bogus"][..],
        &["--wat"],
        &["--hook-event"],
    ] {
        assert_fail_closed(&run_shim_as(args, Some("a"), Some(&fake.socket), EDIT));
    }
    // A report event a later build handles must still never block.
    let out = run_shim_as(
        &["--hook-event", "Stop"],
        Some("a"),
        Some(&fake.socket),
        "{}",
    );
    assert_report_failed(&out);
    assert_eq!(fake.reports().len() + fake.calls().len(), 0);
}

/// The registered event fixes the exit code even when arguments follow it. The command line is
/// refused before any contact, so the socket path is never opened and need not exist.
#[test]
fn trailing_arguments_keep_the_registered_events_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("never-bound.sock");
    for event in ["PostToolUse", "PostToolUseFailure", "Stop", "SessionEnd"] {
        let out = run_shim_as(
            &["--hook-event", event, "extra"],
            Some("a"),
            Some(&socket),
            POST,
        );
        assert_report_failed(&out);
        assert!(stderr(&out).contains("unexpected"), "{}", stderr(&out));
    }
    for args in [
        &["--hook-event", "PreToolUse", "extra"][..],
        &["--hook-event", "Bogus", "extra"],
        &["--hook-event=PostToolUse", "extra"],
    ] {
        assert_fail_closed(&run_shim_as(args, Some("a"), Some(&socket), EDIT));
    }
    assert!(!socket.exists());
}
