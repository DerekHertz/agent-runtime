//! The RPCs the shim makes over the coordinator's Unix domain socket: `BeforeTool` for a
//! PreToolUse gate and `AfterTool` for a PostToolUse or PostToolUseFailure report.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use hyper_util::rt::TokioIo;
use runtime_proto::v1::{
    coordinator_client::CoordinatorClient, Admission, AfterToolReport, BeforeToolRequest,
};
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

/// Longest the shim waits to connect to the coordinator. A local socket either accepts at once or
/// is dead, so this is short.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest a whole `BeforeTool` call (connect plus RPC) may take.
///
/// The spec sets Claude Code's hook timeout to the Defer timeout plus 30 s, 90 s with the default
/// 60 s Defer timeout. The shim must give up and exit 2 itself before that: a hook killed by a
/// timeout fails open (M0 experiment 3). Defer is not produced yet, so today this bounds a hung
/// coordinator. 80 s leaves 10 s of margin under the default.
const CALL_TIMEOUT: Duration = Duration::from_secs(80);

/// Longest a whole `AfterTool` report (connect plus RPC) may take. A report never waits on a
/// lease, so it only bounds a hung coordinator. The documented PostToolUse registration
/// (`docs/hook-registration.md`) sets a 30 s hook timeout, so the shim gives up and reports its
/// own failure (exit 1) well before Claude Code would kill it.
pub const REPORT_TIMEOUT: Duration = Duration::from_secs(10);

async fn connect(socket: PathBuf) -> Result<CoordinatorClient<Channel>> {
    // The endpoint URI is required by tonic but never dialled; the connector below ignores it and
    // opens the Unix socket instead.
    let channel = Endpoint::try_from("http://[::]:50051")?
        .connect_timeout(CONNECT_TIMEOUT)
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move { Ok::<_, std::io::Error>(TokioIo::new(UnixStream::connect(socket).await?)) }
        }))
        .await
        .context("could not connect to the coordinator")?;
    Ok(CoordinatorClient::new(channel))
}

async fn bounded<T>(limit: Duration, call: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(limit, call)
        .await
        .map_err(|_| anyhow!("the coordinator did not answer within {limit:?}"))?
}

/// Ask the coordinator to admit the call.
pub async fn before_tool(socket: PathBuf, request: BeforeToolRequest) -> Result<Admission> {
    bounded(CALL_TIMEOUT, async {
        let admission = connect(socket)
            .await?
            .before_tool(request)
            .await
            .map_err(|status| anyhow!("the coordinator's BeforeTool call failed: {status}"))?;
        Ok(admission.into_inner())
    })
    .await
}

/// Report a completed call. `Ok` only once the coordinator has recorded it.
pub async fn after_tool(socket: PathBuf, report: AfterToolReport) -> Result<()> {
    bounded(REPORT_TIMEOUT, async {
        connect(socket)
            .await?
            .after_tool(report)
            .await
            .map_err(|status| anyhow!("the coordinator's AfterTool call failed: {status}"))?;
        Ok(())
    })
    .await
}
