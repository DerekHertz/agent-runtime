//! Adapter test: the coordinator over tonic on a Unix socket, called as the shim calls it, with the
//! real SQLite trace.

use std::path::Path;
use std::time::Duration;

use clock::Clock;
use coordinator::adapter::{serve, ServeConfig};
use hyper_util::rt::TokioIo;
use runtime_proto::v1::admission::Verdict;
use runtime_proto::v1::coordinator_client::CoordinatorClient;
use runtime_proto::v1::BeforeToolRequest;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;
use trace_store::{read_events, EventKind};

async fn connect(socket: &Path) -> CoordinatorClient<Channel> {
    let socket = socket.to_path_buf();
    // The URI is required by tonic but ignored by the connector.
    let channel = Endpoint::try_from("http://coordinator.invalid")
        .unwrap()
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move { Ok::<_, std::io::Error>(TokioIo::new(UnixStream::connect(socket).await?)) }
        }))
        .await
        .unwrap();
    CoordinatorClient::new(channel)
}

/// A call from the session of `slot`, named by its own `tool_use_id`.
fn request(slot: &str, tool: &str, tool_use_id: &str) -> BeforeToolRequest {
    BeforeToolRequest {
        slot: slot.into(),
        session_id: format!("sess-of-{slot}"),
        tool: tool.into(),
        tool_input_json: r#"{"file_path":"src/lib.rs"}"#.into(),
        intent: String::new(),
        tool_use_id: tool_use_id.into(),
    }
}

#[tokio::test]
async fn before_tool_is_allowed_stamped_and_appended_to_the_trace() {
    // Unix socket paths are limited to about 104 bytes on macOS, so keep the names short.
    let tmp = tempfile::Builder::new().prefix("art").tempdir().unwrap();
    let run_dir = tmp.path().join("run");
    let socket = tmp.path().join("c.sock");

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let coordinator = tokio::spawn(serve(
        ServeConfig {
            run_dir: run_dir.clone(),
            socket: socket.clone(),
            slots: vec!["slot-1".into(), "slot-2".into()],
        },
        async {
            let _ = stopped.await;
        },
    ));

    let mut coordinator = coordinator;
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        if coordinator.is_finished() {
            panic!(
                "coordinator exited before binding: {:?}",
                (&mut coordinator).await
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        socket.exists(),
        "coordinator never bound {}",
        socket.display()
    );

    let mut client = connect(&socket).await;
    let adm = client
        .before_tool(request("slot-1", "Edit", "toolu_1"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(adm.verdict, Verdict::Allow as i32);
    assert!(adm.reason.is_empty());

    let events = read_events(&run_dir.join("trace.db")).unwrap();
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.kind, EventKind::BeforeTool);
    assert_eq!(e.slot, "slot-1");
    assert_eq!(e.session_id, "sess-of-slot-1");
    assert_eq!(e.tool_use_id.as_deref(), Some("toolu_1"));
    assert_eq!(e.tool.as_deref(), Some("Edit"));
    assert_eq!(Clock::from_bytes(&e.vclock).unwrap().entries(), &[1, 0]);
    assert!(e.wall_time > 1_600_000_000_000, "wall_time {}", e.wall_time);

    // An unknown slot is denied and leaves no event.
    let adm = client
        .before_tool(request("slot-9", "Edit", "toolu_2"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(adm.verdict, Verdict::Deny as i32);
    assert!(adm.reason.contains("slot-9"));
    assert_eq!(read_events(&run_dir.join("trace.db")).unwrap().len(), 1);

    // A second slot gets its own clock.
    client
        .before_tool(request("slot-2", "Read", "toolu_3"))
        .await
        .unwrap();
    let events = read_events(&run_dir.join("trace.db")).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].event_id, 2);
    assert_eq!(
        Clock::from_bytes(&events[1].vclock).unwrap().entries(),
        &[0, 1]
    );

    drop(client);
    stop.send(()).unwrap();
    coordinator.await.unwrap().unwrap();
    assert!(
        !socket.exists(),
        "socket file should be removed on shutdown"
    );
}
