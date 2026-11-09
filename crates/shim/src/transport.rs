//! The one RPC the shim makes: `BeforeTool` over the coordinator's Unix domain socket.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use hyper_util::rt::TokioIo;
use runtime_proto::v1::{coordinator_client::CoordinatorClient, Admission, BeforeToolRequest};
use tokio::net::UnixStream;
use tonic::transport::{Endpoint, Uri};
use tower::service_fn;

/// Longest the shim waits to connect to the coordinator. A local socket either accepts at once or
/// is dead, so this is short.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest the whole call (connect plus RPC) may take.
///
/// The spec sets Claude Code's hook timeout to the Defer timeout plus 30 s, 90 s with the default
/// 60 s Defer timeout. The shim must give up and exit 2 itself before that: a hook killed by a
/// timeout fails open (M0 experiment 3). Defer is not produced yet, so today this bounds a hung
/// coordinator. 80 s leaves 10 s of margin under the default.
const CALL_TIMEOUT: Duration = Duration::from_secs(80);

/// Ask the coordinator to admit the call.
pub async fn before_tool(socket: PathBuf, request: BeforeToolRequest) -> Result<Admission> {
    let call = async {
        // The endpoint URI is required by tonic but never dialled; the connector below ignores it
        // and opens the Unix socket instead.
        let channel =
            Endpoint::try_from("http://[::]:50051")?
                .connect_timeout(CONNECT_TIMEOUT)
                .connect_with_connector(service_fn(move |_: Uri| {
                    let socket = socket.clone();
                    async move {
                        Ok::<_, std::io::Error>(TokioIo::new(UnixStream::connect(socket).await?))
                    }
                }))
                .await
                .context("could not connect to the coordinator")?;
        let admission = CoordinatorClient::new(channel)
            .before_tool(request)
            .await
            .map_err(|status| anyhow!("the coordinator's BeforeTool call failed: {status}"))?;
        Ok(admission.into_inner())
    };
    tokio::time::timeout(CALL_TIMEOUT, call)
        .await
        .map_err(|_| anyhow!("the coordinator did not answer within {CALL_TIMEOUT:?}"))?
}
