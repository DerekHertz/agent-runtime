//! `art-coordinator --run-id <id> --slots slot-1,slot-2 [--runs-dir <dir>] [--socket <path>]`

use std::path::PathBuf;

use anyhow::{bail, Context};
use coordinator::adapter::{bind, ServeConfig};

const USAGE: &str = "usage: art-coordinator --run-id <id> --slots slot-1,slot-2 \
[--runs-dir <dir, default \"runs\">] [--socket <path, default <runs-dir>/<run-id>/coordinator.sock>] \
[--leases: a write takes the file's lease, and another slot's write to it is denied]";

#[derive(Debug, PartialEq, Eq)]
struct Args {
    run_id: String,
    slots: Vec<String>,
    runs_dir: PathBuf,
    socket: Option<PathBuf>,
    leases: bool,
}

/// `Ok(None)` means help was requested.
fn parse_args(args: impl IntoIterator<Item = String>) -> anyhow::Result<Option<Args>> {
    let mut run_id = None;
    let mut slots = None;
    let mut runs_dir = None;
    let mut socket = None;
    let mut leases = false;
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        if flag == "--help" || flag == "-h" {
            return Ok(None);
        }
        if flag == "--leases" {
            leases = true;
            continue;
        }
        let mut value = || args.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--run-id" => run_id = Some(value()?),
            "--slots" => slots = Some(value()?),
            "--runs-dir" => runs_dir = Some(PathBuf::from(value()?)),
            "--socket" => socket = Some(PathBuf::from(value()?)),
            other => bail!("unknown argument {other:?}"),
        }
    }
    let run_id = run_id.context("--run-id is required")?;
    if run_id.is_empty() || run_id.contains(['/', '\\']) || run_id == "." || run_id == ".." {
        bail!("--run-id must be a plain directory name, got {run_id:?}");
    }
    let slots = slots
        .context("--slots is required")?
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    Ok(Some(Args {
        run_id,
        slots,
        runs_dir: runs_dir.unwrap_or_else(|| PathBuf::from("runs")),
        socket,
        leases,
    }))
}

async fn shutdown_signal(mut term: tokio::signal::unix::Signal) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return Ok(());
        }
        Err(e) => {
            eprintln!("art-coordinator: {e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    // Install before binding, so SIGTERM always shuts down cleanly and removes the socket.
    let term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("installing the SIGTERM handler")?;
    let run_dir = args.runs_dir.join(&args.run_id);
    let socket = args
        .socket
        .unwrap_or_else(|| run_dir.join("coordinator.sock"));
    let listening = bind(ServeConfig {
        run_dir,
        socket: socket.clone(),
        slots: args.slots,
        leases: args.leases,
    })?;
    eprintln!("art-coordinator: listening on {}", socket.display());
    listening.serve(shutdown_signal(term)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Option<Args>> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_required_and_defaults() {
        let a = parse(&["--run-id", "r1", "--slots", "slot-1,slot-2"])
            .unwrap()
            .unwrap();
        assert_eq!(a.run_id, "r1");
        assert_eq!(a.slots, vec!["slot-1", "slot-2"]);
        assert_eq!(a.runs_dir, PathBuf::from("runs"));
        assert_eq!(a.socket, None);
    }

    #[test]
    fn parses_overrides() {
        let a = parse(&[
            "--run-id",
            "r1",
            "--slots",
            "a",
            "--runs-dir",
            "/x",
            "--socket",
            "/tmp/s",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(a.runs_dir, PathBuf::from("/x"));
        assert_eq!(a.socket, Some(PathBuf::from("/tmp/s")));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(&["--slots", "a"]).is_err());
        assert!(parse(&["--run-id", "r"]).is_err());
        assert!(parse(&["--run-id", "../x", "--slots", "a"]).is_err());
        assert!(parse(&["--run-id", "r", "--slots"]).is_err());
        assert!(parse(&["--run-id", "r", "--slots", "a", "--wat"]).is_err());
        assert!(parse(&["--help"]).unwrap().is_none());
    }
}
