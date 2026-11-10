//! The run's coordinator process: start it, wait until it listens, stop it.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

const READY_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// A running `art-coordinator`. Dropping it without [`Coordinator::stop`] kills the process, so
/// a failed launch leaves none behind.
#[derive(Debug)]
pub struct Coordinator {
    child: Child,
    pub socket: PathBuf,
}

impl Coordinator {
    /// Start `program` for run `run_id` under `runs_dir` and wait until its socket accepts a
    /// connection. With `leases`, writes take leases. Its stderr goes to `log`.
    pub fn start(
        program: &Path,
        runs_dir: &Path,
        run_id: &str,
        slots: &[&str],
        socket: PathBuf,
        leases: bool,
        log: &Path,
    ) -> Result<Coordinator> {
        let log_file = std::fs::File::create(log)
            .with_context(|| format!("could not create {}", log.display()))?;
        let child = Command::new(program)
            .args(["--run-id", run_id, "--slots", &slots.join(",")])
            .arg("--runs-dir")
            .arg(runs_dir)
            .arg("--socket")
            .arg(&socket)
            .args(leases.then_some("--leases"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log_file)
            .spawn()
            .with_context(|| format!("could not start {}", program.display()))?;
        let mut coordinator = Coordinator { child, socket };
        let deadline = Instant::now() + READY_TIMEOUT;
        while UnixStream::connect(&coordinator.socket).is_err() {
            if let Some(status) = coordinator.child.try_wait()? {
                bail!(
                    "the coordinator exited before listening ({status}); see {}",
                    log.display()
                );
            }
            if Instant::now() > deadline {
                bail!(
                    "the coordinator did not listen within {READY_TIMEOUT:?}; see {}",
                    log.display()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(coordinator)
    }

    /// Ask the coordinator to shut down (SIGTERM) and wait for it. Returns whether it exited
    /// cleanly on its own.
    pub fn stop(mut self) -> Result<bool> {
        let pid = self.child.id().to_string();
        let _ = Command::new("kill").args(["-TERM", &pid]).status();
        let deadline = Instant::now() + STOP_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status.success());
            }
            if Instant::now() > deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
