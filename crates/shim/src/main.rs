//! `art-shim`: the Claude Code hook program (SPEC section 3).
//!
//! Claude Code runs it once per hook event and pipes the event JSON to stdin. Outside a run (no
//! slot variables) it does nothing. Inside a run it fails closed: every error, including a panic,
//! ends in exit 2, because Claude Code lets the tool call proceed on a timeout or any other
//! non-zero exit code.

mod config;
mod hook;
mod transport;

use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::ExitCode;

use hook::{Action, Outcome};

fn main() -> ExitCode {
    // A panic would otherwise print its own message and the process would exit 101, which Claude
    // Code treats as a non-blocking error. Silence the default message; `catch_unwind` below turns
    // the panic into exit 2 with an "agent-runtime:" reason.
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = catch_unwind(AssertUnwindSafe(run))
        .unwrap_or_else(|_| Outcome::fail_closed("the shim hit an internal error"));
    emit(&outcome)
}

/// Write the outcome out. A failure to print the decision itself must also block the call.
fn emit(outcome: &Outcome) -> ExitCode {
    let mut code = outcome.code;
    if let Some(stdout) = &outcome.stdout {
        let mut out = std::io::stdout().lock();
        if writeln!(out, "{stdout}")
            .and_then(|()| out.flush())
            .is_err()
        {
            code = 2;
        }
    }
    if let Some(stderr) = &outcome.stderr {
        // Nothing more can be done if stderr is closed.
        let _ = writeln!(std::io::stderr().lock(), "{stderr}");
    }
    ExitCode::from(code)
}

fn run() -> Outcome {
    let config = match config::from_env() {
        Ok(Some(config)) => config,
        // Not in a run: stay out of the way, and never touch the coordinator.
        Ok(None) => return Outcome::silent(),
        Err(e) => return Outcome::fail_closed(format!("{e:#}")),
    };

    let mut stdin = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut stdin) {
        return Outcome::fail_closed(format!("could not read the hook input: {e}"));
    }
    let request = match hook::parse_input(&stdin, &config.slot) {
        Ok(Action::BeforeTool(request)) => request,
        Ok(Action::Ignore) => return Outcome::silent(),
        Err(e) => return Outcome::fail_closed(format!("{e:#}")),
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => return Outcome::fail_closed(format!("could not start the async runtime: {e}")),
    };
    match runtime.block_on(transport::before_tool(config.socket, request)) {
        Ok(admission) => hook::outcome_for(&admission),
        Err(e) => Outcome::fail_closed(format!("{e:#}")),
    }
}
