//! Issue #2 end to end: a recorded PreToolUse JSON piped into the real `art-shim` binary reaches
//! the coordinator's adapter (in-process, `adapter::bind`/`serve`) over its Unix socket, and one
//! stamped event lands in `runs/<id>/trace.db`. The `art-coordinator` binary path is driven by the
//! verify skill in `.agents/skills/`.

use std::io::Write;
use std::process::{Command, Output, Stdio};

use clock::Clock;
use coordinator::adapter::{bind, ServeConfig};
use serde_json::Value;
use trace_store::{read_events, EventKind, TRACE_FILE};

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
    assert_eq!(event.tool.as_deref(), Some("Edit"));
    // The fixture's raw tool_input survives the whole path byte for byte (SPEC section 7).
    #[derive(serde::Deserialize)]
    struct Raw<'a> {
        #[serde(borrow)]
        tool_input: &'a serde_json::value::RawValue,
    }
    let raw: Raw = serde_json::from_str(EDIT).unwrap();
    assert_eq!(event.tool_input.as_deref(), Some(raw.tool_input.get()));
    // Stamped with slot-2's clock after its first event: [0, 1].
    let stamp = Clock::from_bytes(&event.vclock).unwrap();
    assert_eq!(stamp.entries(), &[0, 1]);
}
