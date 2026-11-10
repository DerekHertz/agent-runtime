//! Issue #3 acceptance end to end: hook JSON piped into the real `art-shim` binary under each
//! registration reaches the real coordinator over its Unix socket and lands in
//! `runs/<id>/trace.db`, with payloads in blobs checked by blake3 here.
//!
//! Covers, in one run: PreToolUse for Edit, Write, NotebookEdit, Read and Bash (Write and
//! NotebookEdit fixtures are synthetic, see `fixtures/README.md`); a PostToolUse with a raw
//! `null` response and a changed reported input; a PostToolUseFailure with `is_interrupt`;
//! overlapping calls completed in reverse order and an equal `tool_use_id` in another session
//! (a simulated overlap, not a live parallel run); Stop and SessionEnd; late completions after
//! each; the ended session refused on another slot; a new session on the same slot continuing
//! its clock; input and output blob deduplication; malformed and refused reports exiting 1; and a
//! failed blob write and a failed INSERT whose retries consume no id, tick or binding.

use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use clock::Clock;
use coordinator::adapter::{bind, ServeConfig};
use serde_json::Value;
use trace_store::{
    read_events, read_input_payload, read_payload, BlobHash, Event, EventKind, Outcome, BLOBS_DIR,
    TRACE_FILE,
};

const EDIT: &str = include_str!("fixtures/pre_tool_use_edit.json");
const WRITE: &str = include_str!("fixtures/pre_tool_use_write_synthetic.json");
const NOTEBOOK: &str = include_str!("fixtures/pre_tool_use_notebook_edit_synthetic.json");
const READ: &str = include_str!("fixtures/pre_tool_use_read.json");
const BASH: &str = include_str!("fixtures/pre_tool_use_bash.json");
const POST: &str = include_str!("fixtures/post_tool_use_edit.json");
const FAILURE: &str = include_str!("fixtures/post_tool_use_failure_bash.json");
const STOP: &str = include_str!("fixtures/stop.json");
const SESSION_END: &str = include_str!("fixtures/session_end.json");

const GATE: &[&str] = &[];
const SUCCESS: &[&str] = &["--hook-event", "PostToolUse"];
const FAILED: &[&str] = &["--hook-event", "PostToolUseFailure"];
const STOPPED: &[&str] = &["--hook-event", "Stop"];
const ENDED: &[&str] = &["--hook-event", "SessionEnd"];

fn shim(args: &[&str], slot: &str, socket: &Path, stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_art-shim"))
        .args(args)
        .env("AGENT_RUNTIME_SLOT", slot)
        .env("AGENT_RUNTIME_SOCKET", socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn art-shim");
    match child.stdin.take().unwrap().write_all(stdin.as_bytes()) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
        Err(e) => panic!("write stdin: {e}"),
    }
    child.wait_with_output().unwrap()
}

/// The fixture with its top-level `session_id` and (when it has one) `tool_use_id` swapped by
/// text, and any other `(from, to)` text edits applied, so every other byte stays as recorded.
fn hook(fixture: &str, session: &str, call: Option<&str>, edits: &[(&str, &str)]) -> String {
    let parsed: Value = serde_json::from_str(fixture).unwrap();
    let mut text = fixture.trim_end().to_owned();
    let mut swaps = vec![("session_id", session)];
    swaps.extend(call.map(|c| ("tool_use_id", c)));
    for (field, value) in swaps {
        let old = format!(r#""{field}":{}"#, parsed[field]);
        assert_eq!(text.matches(&old).count(), 1, "{old}");
        text = text.replace(&old, &format!(r#""{field}":"{value}""#));
    }
    for (from, to) in edits {
        assert_eq!(text.matches(from).count(), 1, "{from}");
        text = text.replace(from, to);
    }
    text
}

/// The raw JSON text of a top-level field, as the fixture has it.
fn raw<'a>(fixture: &'a str, field: &str) -> &'a str {
    use serde::Deserialize;
    let key = format!(r#""{field}":"#);
    assert_eq!(fixture.matches(&key).count(), 1, "{key}");
    let start = fixture.find(&key).unwrap() + key.len();
    let mut de = serde_json::Deserializer::from_str(&fixture[start..]);
    <&serde_json::value::RawValue>::deserialize(&mut de)
        .unwrap()
        .get()
}

fn allowed(out: &Output) -> bool {
    let decision: Option<Value> = serde_json::from_slice(&out.stdout).ok();
    out.status.code() == Some(0)
        && decision.is_some_and(|d| d["hookSpecificOutput"]["permissionDecision"] == "allow")
}

fn denied_because(out: &Output, why: &str) -> bool {
    let decision: Option<Value> = serde_json::from_slice(&out.stdout).ok();
    out.status.code() == Some(0)
        && decision.is_some_and(|d| {
            d["hookSpecificOutput"]["permissionDecision"] == "deny"
                && d["hookSpecificOutput"]["permissionDecisionReason"]
                    .as_str()
                    .is_some_and(|r| r.contains(why))
        })
}

fn silent(out: &Output) -> bool {
    out.status.code() == Some(0) && out.stdout.is_empty() && out.stderr.is_empty()
}

fn failed_because(out: &Output, why: &str) -> bool {
    let stderr = String::from_utf8_lossy(&out.stderr);
    out.status.code() == Some(1)
        && out.stdout.is_empty()
        && stderr.starts_with("agent-runtime:")
        && stderr.contains(why)
}

fn stamp(e: &Event) -> Vec<u64> {
    Clock::from_bytes(&e.vclock).unwrap().entries().to_vec()
}

/// The exact payload bytes behind `hash`, read back through the trace store (which rehashes them)
/// and checked against blake3 here too.
fn blob_of(run_dir: &Path, e: &Event, input: bool) -> String {
    let (bytes, hash) = if input {
        (read_input_payload(run_dir, e), &e.input_payload_hash)
    } else {
        (read_payload(run_dir, e), &e.payload_hash)
    };
    let bytes = bytes.unwrap().expect("a blob");
    assert_eq!(
        hash.as_deref(),
        Some(&BlobHash::of(&bytes).as_bytes()[..]),
        "row {} names the blake3 hash of its bytes",
        e.event_id
    );
    String::from_utf8(bytes).unwrap()
}

struct Step {
    label: &'static str,
    out: Output,
}

#[tokio::test(flavor = "multi_thread")]
async fn every_issue_3_hook_event_reaches_the_trace() {
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

    let dir = run_dir.clone();
    let steps = tokio::task::spawn_blocking(move || drive(&socket, &dir))
        .await
        .unwrap();
    let _ = stop.send(());
    coordinator.await.unwrap().unwrap();

    let mut by_label = std::collections::HashMap::new();
    for step in &steps {
        by_label.insert(step.label, &step.out);
    }
    let out = |label: &str| by_label[label];
    for label in [
        "pre-edit",
        "pre-write",
        "pre-notebook",
        "pre-read",
        "pre-bash",
        "pre-o1",
        "pre-o2",
        "pre-o1-other-session",
        "pre-new-session",
        "pre-after-sql-failure",
    ] {
        assert!(allowed(out(label)), "{label}: {:?}", out(label));
    }
    for label in [
        "post-edit-null",
        "fail-bash-interrupted",
        "post-o2",
        "post-o1",
        "post-o1-other-session",
        "stop",
        "late-post-write",
        "session-end",
        "late-post-read",
        "session-end-retry",
    ] {
        let o = out(label);
        assert!(silent(o), "{label}: {}", String::from_utf8_lossy(&o.stderr));
    }
    assert!(denied_because(
        out("pre-ended-session-other-slot"),
        "bound to slot"
    ));
    for (label, why) in [
        ("stop-ended-session-other-slot", "bound to slot"),
        ("stop-unknown-slot", "unknown slot"),
        ("stop-not-json", "hook input"),
        ("session-end-mistyped-reason", "reason"),
        ("post-mismatched-event", "registered for"),
        ("session-end-blob-fails", "blob"),
        ("stop-insert-fails", "trace append failed"),
    ] {
        assert!(failed_because(out(label), why), "{label}: {:?}", out(label));
    }

    let rows = read_events(&run_dir.join(TRACE_FILE)).unwrap();
    let s1 = "sess-accept-a";
    type Row<'a> = (
        u64,
        EventKind,
        &'a str,
        Option<&'a str>,
        Option<u64>,
        Vec<u64>,
    );
    let got: Vec<Row> = rows
        .iter()
        .map(|e| {
            (
                e.event_id,
                e.kind,
                e.session_id.as_str(),
                e.tool_use_id.as_deref(),
                e.before_event_id,
                stamp(e),
            )
        })
        .collect();
    use EventKind::{AfterTool as A, BeforeTool as B, SessionEnd as End, Stop};
    let want: Vec<Row> = vec![
        (1, B, s1, Some("t-edit"), None, vec![1, 0]),
        (2, B, s1, Some("t-write"), None, vec![2, 0]),
        (3, B, s1, Some("t-notebook"), None, vec![3, 0]),
        (4, B, s1, Some("t-read"), None, vec![4, 0]),
        (5, B, s1, Some("t-bash"), None, vec![5, 0]),
        (6, A, s1, Some("t-edit"), Some(1), vec![6, 0]),
        (7, A, s1, Some("t-bash"), Some(5), vec![7, 0]),
        (8, B, s1, Some("t-o1"), None, vec![8, 0]),
        (9, B, s1, Some("t-o2"), None, vec![9, 0]),
        (10, A, s1, Some("t-o2"), Some(9), vec![10, 0]),
        (11, A, s1, Some("t-o1"), Some(8), vec![11, 0]),
        (12, B, "sess-accept-b", Some("t-o1"), None, vec![0, 1]),
        (13, A, "sess-accept-b", Some("t-o1"), Some(12), vec![0, 2]),
        (14, Stop, s1, None, None, vec![12, 0]),
        (15, A, s1, Some("t-write"), Some(2), vec![13, 0]),
        (16, End, s1, None, None, vec![14, 0]),
        (17, A, s1, Some("t-read"), Some(4), vec![15, 0]),
        (18, B, "sess-accept-c", Some("t-edit"), None, vec![16, 0]),
        (19, End, "sess-accept-d", None, None, vec![0, 3]),
        (
            20,
            B,
            "sess-accept-e",
            Some("t-after-sql"),
            None,
            vec![17, 0],
        ),
    ];
    assert_eq!(got, want);

    // Tool rows keep their tool and outcome; the failure keeps its interruption flag.
    let tools: Vec<Option<&str>> = rows[..5].iter().map(|e| e.tool.as_deref()).collect();
    assert_eq!(
        tools,
        [
            Some("Edit"),
            Some("Write"),
            Some("NotebookEdit"),
            Some("Read"),
            Some("Bash")
        ]
    );
    assert_eq!(rows[5].outcome, Some(Outcome::Success));
    assert_eq!(
        rows[6].outcome,
        Some(Outcome::Failure {
            is_interrupt: Some(true)
        })
    );
    for lifecycle in [&rows[13], &rows[15], &rows[18]] {
        assert_eq!(
            (
                &lifecycle.tool,
                lifecycle.outcome,
                &lifecycle.input_payload_hash
            ),
            (&None, None, &None)
        );
    }

    // Payloads, byte for byte and blake3-checked.
    let d = run_dir.as_path();
    for (row, fixture) in [(0, EDIT), (1, WRITE), (2, NOTEBOOK), (3, READ), (4, BASH)] {
        assert_eq!(blob_of(d, &rows[row], false), raw(fixture, "tool_input"));
    }
    assert_eq!(blob_of(d, &rows[5], false), "null", "a raw null response");
    let post_input = raw(POST, "tool_input");
    assert_ne!(post_input, raw(EDIT, "tool_input"));
    assert_eq!(
        blob_of(d, &rows[5], true),
        post_input,
        "the changed input as reported"
    );
    assert_eq!(blob_of(d, &rows[6], false), r#""Interrupted by user""#);
    assert_eq!(blob_of(d, &rows[6], true), raw(FAILURE, "tool_input"));
    assert_eq!(
        blob_of(d, &rows[13], false),
        r#"{"stop_hook_active":false,"last_assistant_message":"done"}"#
    );
    assert_eq!(blob_of(d, &rows[15], false), r#"{"reason":"other"}"#);
    assert_eq!(blob_of(d, &rows[18], false), r#"{"reason":"clear"}"#);

    // Deduplication: equal inputs share one blob across events and sessions, and so do equal
    // outputs; distinct bytes never share.
    assert_eq!(
        rows[17].payload_hash, rows[0].payload_hash,
        "same Edit input, new session"
    );
    for paired in [9, 10, 12] {
        assert_eq!(rows[paired].input_payload_hash, rows[5].input_payload_hash);
        assert_eq!(
            rows[paired].payload_hash, rows[9].payload_hash,
            "same response"
        );
    }
    assert_ne!(rows[5].payload_hash, rows[9].payload_hash);
    let mut named: Vec<String> = rows
        .iter()
        .flat_map(|e| [&e.payload_hash, &e.input_payload_hash])
        .flatten()
        .map(|h| h.iter().map(|b| format!("{b:02x}")).collect())
        .collect();
    named.sort();
    named.dedup();
    let mut files: Vec<String> = std::fs::read_dir(run_dir.join(BLOBS_DIR))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    // The failed INSERT left one orphan blob, which nothing names; everything named exists.
    let orphans: Vec<&String> = files.iter().filter(|f| !named.contains(f)).collect();
    let stop_e = r#"{"stop_hook_active":true}"#;
    let orphan = BlobHash::of(stop_e.as_bytes());
    let orphan_hex: String = orphan
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(orphans, [&orphan_hex]);
    assert!(named.iter().all(|n| files.contains(n)));
}

/// Every hook call of the run, in order, with its label.
fn drive(socket: &Path, run_dir: &Path) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut run = |label: &'static str, args: &[&str], slot: &str, stdin: String| {
        let out = shim(args, slot, socket, &stdin);
        steps.push(Step { label, out });
    };
    let a = "sess-accept-a";
    let response = (
        r#""tool_response":{"filePath":"/work/src/lib.rs"}"#,
        r#""tool_response":null"#,
    );
    // Each gated tool once; Write and NotebookEdit are synthetic fixtures.
    run(
        "pre-edit",
        GATE,
        "slot-1",
        hook(EDIT, a, Some("t-edit"), &[]),
    );
    run(
        "pre-write",
        GATE,
        "slot-1",
        hook(WRITE, a, Some("t-write"), &[]),
    );
    run(
        "pre-notebook",
        GATE,
        "slot-1",
        hook(NOTEBOOK, a, Some("t-notebook"), &[]),
    );
    run(
        "pre-read",
        GATE,
        "slot-1",
        hook(READ, a, Some("t-read"), &[]),
    );
    run(
        "pre-bash",
        GATE,
        "slot-1",
        hook(BASH, a, Some("t-bash"), &[]),
    );
    // A raw null response with the reported input changed from the PreToolUse's.
    run(
        "post-edit-null",
        SUCCESS,
        "slot-1",
        hook(POST, a, Some("t-edit"), &[response]),
    );
    let interrupted = (r#""is_interrupt":false"#, r#""is_interrupt":true"#);
    let error = (raw(FAILURE, "error"), r#""Interrupted by user""#);
    run(
        "fail-bash-interrupted",
        FAILED,
        "slot-1",
        hook(FAILURE, a, Some("t-bash"), &[interrupted, error]),
    );
    // Overlapping calls, completed in reverse order; then the same id in another session.
    run("pre-o1", GATE, "slot-1", hook(EDIT, a, Some("t-o1"), &[]));
    run("pre-o2", GATE, "slot-1", hook(EDIT, a, Some("t-o2"), &[]));
    run(
        "post-o2",
        SUCCESS,
        "slot-1",
        hook(POST, a, Some("t-o2"), &[]),
    );
    run(
        "post-o1",
        SUCCESS,
        "slot-1",
        hook(POST, a, Some("t-o1"), &[]),
    );
    let b = "sess-accept-b";
    run(
        "pre-o1-other-session",
        GATE,
        "slot-2",
        hook(EDIT, b, Some("t-o1"), &[]),
    );
    run(
        "post-o1-other-session",
        SUCCESS,
        "slot-2",
        hook(POST, b, Some("t-o1"), &[]),
    );
    // Stop, a late completion, SessionEnd, another late completion.
    run("stop", STOPPED, "slot-1", hook(STOP, a, None, &[]));
    let as_write = (r#""tool_name":"Edit""#, r#""tool_name":"Write""#);
    run(
        "late-post-write",
        SUCCESS,
        "slot-1",
        hook(POST, a, Some("t-write"), &[as_write]),
    );
    run(
        "session-end",
        ENDED,
        "slot-1",
        hook(SESSION_END, a, None, &[]),
    );
    let as_read = (r#""tool_name":"Edit""#, r#""tool_name":"Read""#);
    run(
        "late-post-read",
        SUCCESS,
        "slot-1",
        hook(POST, a, Some("t-read"), &[as_read]),
    );
    // The ended session stays on slot-1.
    run(
        "pre-ended-session-other-slot",
        GATE,
        "slot-2",
        hook(EDIT, a, Some("t-moved"), &[]),
    );
    run(
        "stop-ended-session-other-slot",
        STOPPED,
        "slot-2",
        hook(STOP, a, None, &[]),
    );
    // A new session on slot-1 continues its clock; an equal tool_use_id is its own call.
    run(
        "pre-new-session",
        GATE,
        "slot-1",
        hook(EDIT, "sess-accept-c", Some("t-edit"), &[]),
    );
    // Refused or unreadable reports: exit 1, nothing recorded.
    run(
        "stop-unknown-slot",
        STOPPED,
        "slot-9",
        hook(STOP, "sess-accept-z", None, &[]),
    );
    run("stop-not-json", STOPPED, "slot-1", "not json".into());
    let mistyped = (r#""reason":"other""#, r#""reason":1"#);
    run(
        "session-end-mistyped-reason",
        ENDED,
        "slot-1",
        hook(SESSION_END, a, None, &[mistyped]),
    );
    run(
        "post-mismatched-event",
        SUCCESS,
        "slot-1",
        hook(STOP, a, None, &[]),
    );
    // A failed blob write: nothing recorded; the retry takes the id and slot-2's next stamp.
    let clear = (r#""reason":"other""#, r#""reason":"clear""#);
    let d_end = hook(SESSION_END, "sess-accept-d", None, &[clear]);
    let blobs = run_dir.join(BLOBS_DIR);
    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o555)).unwrap();
    run("session-end-blob-fails", ENDED, "slot-2", d_end.clone());
    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o755)).unwrap();
    run("session-end-retry", ENDED, "slot-2", d_end);
    // A failed INSERT: the next id is taken behind the coordinator's back, so the Stop of a new
    // session fails after its blob. It binds nothing: the session then starts on slot-1 with
    // the id and the slot-1 stamp the failed report did not use.
    let db = rusqlite::Connection::open(run_dir.join(TRACE_FILE)).unwrap();
    db.execute(
        "INSERT INTO events (event_id, agent_id, session_id, vclock, kind, wall_time)
         VALUES (20, 'planted', 'x', x'', 'BeforeTool', 0)",
        [],
    )
    .unwrap();
    let active = (
        r#""stop_hook_active":false,"last_assistant_message":"done""#,
        r#""stop_hook_active":true"#,
    );
    run(
        "stop-insert-fails",
        STOPPED,
        "slot-2",
        hook(STOP, "sess-accept-e", None, &[active]),
    );
    db.execute_batch("DROP TRIGGER events_no_delete; DELETE FROM events WHERE event_id = 20;")
        .unwrap();
    run(
        "pre-after-sql-failure",
        GATE,
        "slot-1",
        hook(EDIT, "sess-accept-e", Some("t-after-sql"), &[]),
    );
    steps
}
