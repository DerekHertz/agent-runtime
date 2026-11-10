//! PostToolUseFailure end to end: recorded hook JSON piped into the real `art-shim` binary,
//! registered as `--hook-event PostToolUseFailure`, reaches the real coordinator adapter over its
//! Unix socket, and lands in `runs/<id>/trace.db` with its raw error and raw input in blobs.
//! Covers out-of-order pairing, the interruption flag (true, false, omitted), an empty error
//! message, a repeat, an orphan, a tool mismatch, and a failed blob write whose corrected retry
//! takes the same event id and stamp.

use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use clock::Clock;
use coordinator::adapter::{bind, ServeConfig};
use serde_json::Value;
use trace_store::{
    read_events, read_input_payload, read_payload, EventKind, Outcome, BLOBS_DIR, TRACE_FILE,
};

const BASH: &str = include_str!("fixtures/pre_tool_use_bash.json");
const FAILURE: &str = include_str!("fixtures/post_tool_use_failure_bash.json");
const FAILED: &[&str] = &["--hook-event", "PostToolUseFailure"];
const SESSION: &str = "sess-e2e-failure";

fn run_shim_as(args: &[&str], socket: &Path, stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_art-shim"))
        .args(args)
        .env("AGENT_RUNTIME_SLOT", "slot-1")
        .env("AGENT_RUNTIME_SOCKET", socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn art-shim");
    // A refused command line or configuration exits before reading stdin.
    match child.stdin.take().unwrap().write_all(stdin.as_bytes()) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
        Err(e) => panic!("write stdin: {e}"),
    }
    child.wait_with_output().unwrap()
}

/// The fixture with its top-level `session_id` and `tool_use_id` swapped by text, so every other
/// byte (the raw tool_input and error included) stays as recorded.
fn with_ids(fixture: &str, tool_use_id: &str) -> String {
    let hook: Value = serde_json::from_str(fixture).unwrap();
    let mut text = fixture.to_owned();
    for (field, value) in [("session_id", SESSION), ("tool_use_id", tool_use_id)] {
        let old = format!(r#""{field}":{}"#, hook[field]);
        assert_eq!(text.matches(&old).count(), 1, "{old}");
        text = text.replace(&old, &format!(r#""{field}":"{value}""#));
    }
    text
}

/// The failure fixture for `tool_use_id` with `"error":...,"is_interrupt":false` swapped by text
/// for `fields`.
fn failure(tool_use_id: &str, fields: &str) -> String {
    let text = with_ids(FAILURE, tool_use_id);
    let start = text.find(r#""error":"#).unwrap();
    let end = text.find(r#","duration_ms""#).unwrap();
    format!("{}{}{}", &text[..start], fields, &text[end..])
}

fn raw_field<'a>(fixture: &'a str, field: &str) -> &'a str {
    #[derive(serde::Deserialize)]
    struct Raw<'a> {
        #[serde(borrow)]
        tool_input: &'a serde_json::value::RawValue,
        #[serde(borrow)]
        error: Option<&'a serde_json::value::RawValue>,
    }
    let raw: Raw = serde_json::from_str(fixture).unwrap();
    match field {
        "tool_input" => raw.tool_input.get(),
        _ => raw.error.unwrap().get(),
    }
}

fn silent(out: &Output) -> bool {
    out.status.code() == Some(0) && out.stdout.is_empty() && out.stderr.is_empty()
}

fn report_failed(out: &Output) -> bool {
    out.status.code() == Some(1)
        && out.stdout.is_empty()
        && String::from_utf8_lossy(&out.stderr).starts_with("agent-runtime:")
}

fn blob_files(run_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(run_dir.join(BLOBS_DIR))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_and_interrupted_calls_reach_the_trace_with_their_raw_error_and_flag() {
    let tmp = tempfile::Builder::new().prefix("art").tempdir().unwrap();
    let run_dir = tmp.path().join("runs").join("e2e");
    let socket = tmp.path().join("c.sock");
    let listening = bind(ServeConfig {
        run_dir: run_dir.clone(),
        socket: socket.clone(),
        slots: vec!["slot-1".into(), "slot-2".into()],
        leases: false,
    })
    .expect("bind coordinator");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let coordinator = tokio::spawn(listening.serve(async {
        let _ = stopped.await;
    }));

    let blobs = run_dir.join(BLOBS_DIR);
    let shim_socket = socket.clone();
    let outs = tokio::task::spawn_blocking(move || {
        let s = shim_socket.as_path();
        let mut outs = Vec::new();
        for id in ["A", "B"] {
            outs.push(run_shim_as(&[], s, &with_ids(BASH, id)));
        }
        // B fails first (interrupted), then A (the fixture as recorded, is_interrupt false).
        outs.push(run_shim_as(
            FAILED,
            s,
            &failure("B", r#""error":"Interrupted","is_interrupt":true"#),
        ));
        outs.push(run_shim_as(FAILED, s, &with_ids(FAILURE, "A")));
        // A repeat with an empty message and no flag, then an orphan.
        outs.push(run_shim_as(FAILED, s, &failure("A", r#""error":"""#)));
        outs.push(run_shim_as(FAILED, s, &failure("C", r#""error":"lost""#)));
        // A report naming another tool than its call is refused.
        let edit = with_ids(FAILURE, "A").replace(r#""tool_name":"Bash""#, r#""tool_name":"Edit""#);
        outs.push(run_shim_as(FAILED, s, &edit));
        // The blob store refuses a new error blob; the corrected retry is recorded.
        let d = failure("D", r#""error":"D failed","is_interrupt":false"#);
        std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o555)).unwrap();
        outs.push(run_shim_as(FAILED, s, &d));
        std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o755)).unwrap();
        outs.push(run_shim_as(FAILED, s, &d));
        outs
    })
    .await
    .unwrap();

    for out in &outs[..2] {
        assert_eq!(out.status.code(), Some(0));
        let decision: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            decision["hookSpecificOutput"]["permissionDecision"],
            "allow"
        );
    }
    for (i, out) in outs[2..6].iter().enumerate() {
        assert!(
            silent(out),
            "report {i}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mismatch = String::from_utf8_lossy(&outs[6].stderr);
    assert!(
        report_failed(&outs[6]) && mismatch.contains("recorded as"),
        "{mismatch}"
    );
    let blob_error = String::from_utf8_lossy(&outs[7].stderr);
    assert!(
        report_failed(&outs[7]) && blob_error.contains("blob"),
        "{blob_error}"
    );
    assert!(
        silent(&outs[8]),
        "{}",
        String::from_utf8_lossy(&outs[8].stderr)
    );

    let _ = stop.send(());
    coordinator.await.unwrap().unwrap();

    let events = read_events(&run_dir.join(TRACE_FILE)).unwrap();
    let failed = |is_interrupt| Some(Outcome::Failure { is_interrupt });
    let rows: Vec<_> = events
        .iter()
        .map(|e| {
            let stamp = Clock::from_bytes(&e.vclock).unwrap().entries().to_vec();
            let id = e.tool_use_id.clone().unwrap();
            (e.event_id, e.kind, id, e.before_event_id, e.outcome, stamp)
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (1, EventKind::BeforeTool, "A".into(), None, None, vec![1, 0]),
            (2, EventKind::BeforeTool, "B".into(), None, None, vec![2, 0]),
            (
                3,
                EventKind::AfterTool,
                "B".into(),
                Some(2),
                failed(Some(true)),
                vec![3, 0]
            ),
            (
                4,
                EventKind::AfterTool,
                "A".into(),
                Some(1),
                failed(Some(false)),
                vec![4, 0]
            ),
            (
                5,
                EventKind::AfterTool,
                "A".into(),
                Some(1),
                failed(None),
                vec![5, 0]
            ),
            (
                6,
                EventKind::AfterTool,
                "C".into(),
                None,
                failed(None),
                vec![6, 0]
            ),
            (
                7,
                EventKind::AfterTool,
                "D".into(),
                None,
                failed(Some(false)),
                vec![7, 0]
            ),
        ],
        "the refused and the failed reports used no id or stamp"
    );
    let errors: Vec<_> = events[2..]
        .iter()
        .map(|e| String::from_utf8(read_payload(&run_dir, e).unwrap().unwrap()).unwrap())
        .collect();
    let fixture_error = raw_field(FAILURE, "error");
    assert!(
        fixture_error.starts_with(r#""Exit code 1\n"#),
        "{fixture_error}"
    );
    assert_eq!(
        errors,
        [
            r#""Interrupted""#,
            fixture_error,
            r#""""#,
            r#""lost""#,
            r#""D failed""#
        ],
        "each payload is the raw error text exactly as the hook sent it"
    );
    let input = raw_field(FAILURE, "tool_input");
    for e in &events[2..] {
        assert_eq!(e.tool.as_deref(), Some("Bash"));
        assert_eq!(e.session_id, SESSION);
        assert_eq!(
            read_input_payload(&run_dir, e).unwrap().as_deref(),
            Some(input.as_bytes()),
            "the PostToolUseFailure input is kept for paired and orphan reports alike"
        );
    }
    // Every blob is named by a row; the failed write left no temporary file.
    let mut named: Vec<String> = events
        .iter()
        .flat_map(|e| [e.payload_hash.clone(), e.input_payload_hash.clone()])
        .flatten()
        .map(|h| h.iter().map(|b| format!("{b:02x}")).collect())
        .collect();
    named.sort();
    named.dedup();
    assert_eq!(blob_files(&run_dir), named);
}
