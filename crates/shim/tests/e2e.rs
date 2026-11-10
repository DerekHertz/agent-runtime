//! Issue #2 end to end: a recorded PreToolUse JSON piped into the real `art-shim` binary reaches
//! the coordinator's adapter (in-process, `adapter::bind`/`serve`) over its Unix socket, and one
//! stamped event lands in `runs/<id>/trace.db`. The `art-coordinator` binary path is driven by the
//! verify skill in `.agents/skills/`.

use std::io::Write;
use std::process::{Command, Output, Stdio};

use clock::Clock;
use coordinator::adapter::{bind, ServeConfig};
use serde_json::Value;
use trace_store::{read_events, read_payload, BlobHash, EventKind, TRACE_FILE};

const EDIT: &str = include_str!("fixtures/pre_tool_use_edit.json");

fn run_shim(slot: &str, socket: &std::path::Path, stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_art-shim"))
        .env("AGENT_RUNTIME_SLOT", slot)
        .env("AGENT_RUNTIME_SOCKET", socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn art-shim");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn recorded_pre_tool_use_is_allowed_and_one_stamped_event_is_traced() {
    // Short paths: Unix socket paths are limited to about 104 bytes on macOS.
    let tmp = tempfile::Builder::new().prefix("art").tempdir().unwrap();
    let run_dir = tmp.path().join("runs").join("e2e");
    let socket = tmp.path().join("c.sock");

    let listening = bind(ServeConfig {
        run_dir: run_dir.clone(),
        socket: socket.clone(),
        slots: vec!["slot-1".into(), "slot-2".into()],
    })
    .expect("bind coordinator");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let coordinator = tokio::spawn(listening.serve(async {
        let _ = stopped.await;
    }));

    let shim_socket = socket.clone();
    let out = tokio::task::spawn_blocking(move || run_shim("slot-2", &shim_socket, EDIT))
        .await
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed: Value = serde_json::from_slice(&out.stdout).expect("shim prints one JSON object");
    assert_eq!(
        printed,
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow"
            }
        })
    );

    let _ = stop.send(());
    coordinator.await.unwrap().unwrap();

    let events = read_events(&run_dir.join(TRACE_FILE)).unwrap();
    assert_eq!(events.len(), 1, "{events:?}");
    let event = &events[0];
    let fixture: Value = serde_json::from_str(EDIT).unwrap();
    assert_eq!(event.kind, EventKind::BeforeTool);
    assert_eq!(event.slot, "slot-2");
    assert_eq!(event.session_id, fixture["session_id"].as_str().unwrap());
    assert_eq!(
        event.tool_use_id.as_deref(),
        Some(fixture["tool_use_id"].as_str().unwrap())
    );
    assert_eq!(event.tool.as_deref(), Some("Edit"));
    // The fixture's raw tool_input survives the whole path byte for byte (SPEC section 7).
    #[derive(serde::Deserialize)]
    struct Raw<'a> {
        #[serde(borrow)]
        tool_input: &'a serde_json::value::RawValue,
    }
    let raw: Raw = serde_json::from_str(EDIT).unwrap();
    // It lives in the run's blob store; the row holds only its blake3 hash.
    assert_eq!(event.payload, None);
    assert_eq!(
        event.payload_hash.as_deref(),
        Some(&BlobHash::of(raw.tool_input.get().as_bytes()).as_bytes()[..])
    );
    assert_eq!(
        read_payload(&run_dir, event).unwrap().as_deref(),
        Some(raw.tool_input.get().as_bytes())
    );
    // Stamped with slot-2's clock after its first event: [0, 1].
    let stamp = Clock::from_bytes(&event.vclock).unwrap();
    assert_eq!(stamp.entries(), &[0, 1]);
}

const POST: &str = include_str!("fixtures/post_tool_use_edit.json");

fn run_shim_as(args: &[&str], slot: &str, socket: &std::path::Path, stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_art-shim"))
        .args(args)
        .env("AGENT_RUNTIME_SLOT", slot)
        .env("AGENT_RUNTIME_SOCKET", socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn art-shim");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// The fixture with its `tool_use_id` (and, for PostToolUse, its `tool_response`) swapped by
/// text, so every other byte stays as recorded.
fn with_id(fixture: &str, tool_use_id: &str, response: Option<&str>) -> String {
    let fixture_id = r#""tool_use_id":"toolu_01Uy28i9ZxHpdQkCRWhbwK3g""#;
    assert_eq!(fixture.matches(fixture_id).count(), 1);
    let mut text = fixture.replace(fixture_id, &format!(r#""tool_use_id":"{tool_use_id}""#));
    if let Some(response) = response {
        let old = r#""tool_response":{"filePath":"/work/src/lib.rs"}"#;
        assert_eq!(text.matches(old).count(), 1);
        text = text.replace(old, &format!(r#""tool_response":{response}"#));
    }
    text
}

/// PostToolUse end to end: two overlapping Edit calls complete in reverse order, through the real
/// shim binary, the coordinator's socket, the SQLite trace and the blob store.
#[tokio::test(flavor = "multi_thread")]
async fn reversed_post_tool_use_reports_pair_with_their_own_calls() {
    let tmp = tempfile::Builder::new().prefix("art").tempdir().unwrap();
    let run_dir = tmp.path().join("runs").join("e2e");
    let socket = tmp.path().join("c.sock");
    let listening = bind(ServeConfig {
        run_dir: run_dir.clone(),
        socket: socket.clone(),
        slots: vec!["slot-1".into(), "slot-2".into()],
    })
    .expect("bind coordinator");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let coordinator = tokio::spawn(listening.serve(async {
        let _ = stopped.await;
    }));

    let report = &["--hook-event", "PostToolUse"][..];
    let steps = [
        (&[][..], with_id(EDIT, "A", None)),
        (&[][..], with_id(EDIT, "B", None)),
        (report, with_id(POST, "B", Some("null"))),
        (report, with_id(POST, "A", None)),
    ];
    let shim_socket = socket.clone();
    let outs = tokio::task::spawn_blocking(move || {
        steps
            .iter()
            .map(|(args, stdin)| run_shim_as(args, "slot-1", &shim_socket, stdin))
            .collect::<Vec<_>>()
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
    for out in &outs[2..] {
        // A recorded report is silent: exit 0, nothing on stdout or stderr.
        assert_eq!(
            (
                out.status.code(),
                out.stdout.as_slice(),
                out.stderr.as_slice()
            ),
            (Some(0), &b""[..], &b""[..]),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = stop.send(());
    coordinator.await.unwrap().unwrap();

    let events = read_events(&run_dir.join(TRACE_FILE)).unwrap();
    let rows: Vec<_> = events
        .iter()
        .map(|e| {
            let stamp = Clock::from_bytes(&e.vclock).unwrap().entries().to_vec();
            let id = e.tool_use_id.clone().unwrap();
            (e.event_id, e.kind, id, e.before_event_id, stamp)
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (1, EventKind::BeforeTool, "A".into(), None, vec![1, 0]),
            (2, EventKind::BeforeTool, "B".into(), None, vec![2, 0]),
            (3, EventKind::AfterTool, "B".into(), Some(2), vec![3, 0]),
            (4, EventKind::AfterTool, "A".into(), Some(1), vec![4, 0]),
        ]
    );
    let post_input =
        r#"{"file_path":"/work/src/lib.rs","old_string":"a","new_string":"b","replace_all":false}"#;
    assert_eq!(
        read_payload(&run_dir, &events[2]).unwrap().as_deref(),
        Some(&b"null"[..]),
        "an explicit null response is stored as the four bytes null"
    );
    assert_eq!(
        read_payload(&run_dir, &events[3]).unwrap().as_deref(),
        Some(&br#"{"filePath":"/work/src/lib.rs"}"#[..])
    );
    for after in &events[2..] {
        assert_eq!(after.outcome, Some(trace_store::Outcome::Success));
        assert_eq!(
            trace_store::read_input_payload(&run_dir, after)
                .unwrap()
                .as_deref(),
            Some(post_input.as_bytes())
        );
    }
}
