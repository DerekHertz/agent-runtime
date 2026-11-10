//! `art run`: start one run. It copies the target tree into the run directory, starts the
//! coordinator, launches one headless session per slot with the shim registered on every hook
//! event, waits for the sessions, and shuts the coordinator down (SPEC section 3, "Slots and
//! launching").
//!
//! Each step lives in its own module: [`run_file`] is the input, [`settings`] the hook
//! registration, [`coordinator`] the coordinator process and [`session`] one agent session.
//! This file only puts them in order.

mod coordinator;
mod run_file;
mod session;
mod settings;

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde_json::json;

use coordinator::Coordinator;
use run_file::RunFile;
use session::{Launch, Session};

/// Directories of the target that are build output or history, not source.
const NOT_COPIED: [&str; 2] = ["target", ".git"];

/// `art run <run-file> [--run-id <id>] [--runs-dir <dir>]`; `args` starts at the run file.
pub fn run(args: &[String]) -> Result<String> {
    let [run_file, flags @ ..] = args else {
        bail!("run needs a run file");
    };
    let (mut run_id, mut runs_dir) = (None, PathBuf::from("runs"));
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        let value = flags
            .next()
            .with_context(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--run-id" => run_id = Some(value.clone()),
            "--runs-dir" => runs_dir = PathBuf::from(value),
            other => bail!("unknown argument {other:?}"),
        }
    }
    let run_id = match run_id {
        Some(id) => id,
        None => format!(
            "run-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()
        ),
    };
    let file = RunFile::read(Path::new(run_file))?;
    start_run(&file, &std::path::absolute(runs_dir)?, &run_id)
}

fn start_run(file: &RunFile, runs_dir: &Path, run_id: &str) -> Result<String> {
    let run_dir = runs_dir.join(run_id);
    if run_dir.exists() {
        bail!(
            "{} already exists; choose another --run-id",
            run_dir.display()
        );
    }
    let bin_dir = std::env::current_exe()?
        .parent()
        .context("the art binary has no directory")?
        .to_path_buf();
    let (shim, coordinator_bin) = (bin_dir.join("art-shim"), bin_dir.join("art-coordinator"));
    for bin in [&shim, &coordinator_bin] {
        if !bin.is_file() {
            bail!("{} is missing; build the workspace first", bin.display());
        }
    }

    let work_dir = run_dir.join("work");
    copy_tree(&file.target, &work_dir)
        .with_context(|| format!("could not copy {} into the run", file.target.display()))?;
    let work_dir = work_dir.canonicalize()?;
    let settings_path = run_dir.join("settings.json");
    let settings = settings::settings(&shim, file.gate_timeout_s);
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings)? + "\n",
    )?;

    // A Unix socket path is limited to about 100 bytes, so it cannot live in the run directory.
    let socket = PathBuf::from(format!("/tmp/art-{}.sock", std::process::id()));
    let coordinator = Coordinator::start(
        &coordinator_bin,
        runs_dir,
        run_id,
        &file.slot_names(),
        socket,
        &run_dir.join("coordinator.log"),
    )?;
    let launch = Launch {
        agent: &file.agent,
        model: &file.model,
        allowed_tools: &file.allowed_tools,
        settings: &settings_path,
        socket: &coordinator.socket,
        work_dir: &work_dir,
    };
    let started = Instant::now();
    let mut sessions = Vec::new();
    for spec in &file.slots {
        let dir = run_dir.join("sessions").join(&spec.slot);
        sessions.push(session::start(&launch, &spec.slot, &spec.prompt, dir)?);
    }
    let results = wait_all(
        sessions,
        Duration::from_secs(file.session_timeout_s),
        &work_dir,
    )?;
    let clean_shutdown = coordinator.stop()?;

    let summary = json!({
        "run_id": run_id,
        "wall_s": started.elapsed().as_secs_f64(),
        "coordinator_clean_shutdown": clean_shutdown,
        "sessions": results,
    });
    std::fs::write(
        run_dir.join("run.json"),
        serde_json::to_string_pretty(&summary)? + "\n",
    )?;

    let mut out = String::new();
    for r in &results {
        let _ = writeln!(
            out,
            "{}: exit {}, transcript {}",
            r["slot"], r["exit"], r["transcript"]
        );
    }
    let _ = writeln!(out, "coordinator shut down cleanly: {clean_shutdown}");
    let _ = writeln!(out, "run directory: {}", run_dir.display());
    let _ = writeln!(
        out,
        "next: art query {} concurrent-writes",
        run_dir.display()
    );
    Ok(out)
}

/// Wait for every session, killing any still running at `timeout`, then copy each transcript
/// into the session's directory. One JSON record per session.
fn wait_all(
    mut sessions: Vec<Session>,
    timeout: Duration,
    work_dir: &Path,
) -> Result<Vec<serde_json::Value>> {
    let deadline = Instant::now() + timeout;
    let mut exits: Vec<Option<String>> = vec![None; sessions.len()];
    while exits.iter().any(Option::is_none) {
        for (session, exit) in sessions.iter_mut().zip(&mut exits) {
            if exit.is_some() {
                continue;
            }
            if let Some(status) = session.child.try_wait()? {
                *exit = Some(status.code().map_or("signal".into(), |c| c.to_string()));
            } else if Instant::now() > deadline {
                let _ = session.child.kill();
                let _ = session.child.wait();
                *exit = Some("timeout".into());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut results = Vec::new();
    for (session, exit) in sessions.iter().zip(exits) {
        let transcript = home
            .as_deref()
            .map(|home| session::transcript_path(home, work_dir, &session.session_id))
            .filter(|path| path.is_file());
        let copied = match &transcript {
            Some(path) => std::fs::copy(path, session.dir.join("transcript.jsonl")).is_ok(),
            None => false,
        };
        results.push(json!({
            "slot": session.slot,
            "session_id": session.session_id,
            "exit": exit,
            "transcript": if copied { "copied" } else { "not found" },
        }));
    }
    Ok(results)
}

/// Copy the directory `from` to the new directory `to`, leaving out [`NOT_COPIED`] and anything
/// that is neither a file nor a directory.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let (name, kind) = (entry.file_name(), entry.file_type()?);
        let dest = to.join(&name);
        if kind.is_dir() {
            if !NOT_COPIED.iter().any(|skip| name == *skip) {
                copy_tree(&entry.path(), &dest)?;
            }
        } else if kind.is_file() {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_target_is_copied_without_build_output_or_history() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("crate");
        for dir in ["src", "target/debug", ".git"] {
            std::fs::create_dir_all(from.join(dir)).unwrap();
        }
        for file in ["Cargo.toml", "src/lib.rs", "target/debug/x", ".git/HEAD"] {
            std::fs::write(from.join(file), file).unwrap();
        }
        let to = tmp.path().join("work");
        copy_tree(&from, &to).unwrap();
        assert_eq!(
            std::fs::read_to_string(to.join("src/lib.rs")).unwrap(),
            "src/lib.rs"
        );
        assert!(to.join("Cargo.toml").is_file());
        assert!(!to.join("target").exists() && !to.join(".git").exists());
    }
}
