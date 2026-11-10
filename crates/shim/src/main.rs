//! `art-shim`: the Claude Code hook program (SPEC section 3).
//!
//! Claude Code runs it once per hook event and pipes the event JSON to stdin. Outside a run (no
//! slot variables) it does nothing. Inside a run, its registration (`cli`) fixes how it fails,
//! before it reads its configuration or stdin: a PreToolUse gate fails closed, so every error,
//! including a panic, ends in exit 2, because Claude Code lets the tool call proceed on a timeout
//! or any other non-zero exit code. A PostToolUse or PostToolUseFailure report fails with a
//! nonblocking exit 1 and a reason on stderr, and a recorded report exits 0 silently.

mod cli;
mod config;
mod hook;
mod transport;

use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::ExitCode;

use cli::{BadInvocation, Invocation};
use hook::{Action, Outcome, Policy};

fn main() -> ExitCode {
    // A panic would otherwise print its own message and the process would exit 101, which Claude
    // Code treats as a non-blocking error. Silence the default message; `catch_unwind` below turns
    // the panic into the registration's failure with an "agent-runtime:" reason.
    std::panic::set_hook(Box::new(|_| {}));
    let invocation = cli::parse(std::env::args_os().skip(1));
    let policy = match &invocation {
        Ok(invocation) => invocation.policy(),
        Err(bad) => bad.policy,
    };
    let outcome = catch_unwind(AssertUnwindSafe(|| run(invocation, policy)))
        .unwrap_or_else(|_| policy.failure("the shim hit an internal error"));
    emit(&outcome, policy)
}

/// Write the outcome out. A failure to print a gate's decision must also block the call; a report
/// prints nothing on stdout.
fn emit(outcome: &Outcome, policy: Policy) -> ExitCode {
    let mut code = outcome.code;
    if let Some(stdout) = &outcome.stdout {
        let mut out = std::io::stdout().lock();
        if writeln!(out, "{stdout}")
            .and_then(|()| out.flush())
            .is_err()
        {
            code = policy.code();
        }
    }
    if let Some(stderr) = &outcome.stderr {
        // Nothing more can be done if stderr is closed.
        let _ = writeln!(std::io::stderr().lock(), "{stderr}");
    }
    ExitCode::from(code)
}

fn run(invocation: Result<Invocation, BadInvocation>, policy: Policy) -> Outcome {
    let config = match config::from_env() {
        Ok(Some(config)) => config,
        // Not in a run: stay out of the way, and never touch the coordinator, whatever the
        // command line says.
        Ok(None) => return Outcome::silent(),
        Err(e) => return policy.failure(format!("{e:#}")),
    };
    let invocation = match invocation {
        Ok(invocation) => invocation,
        Err(bad) => return policy.failure(bad.reason),
    };

    let mut stdin = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut stdin) {
        return policy.failure(format!("could not read the hook input: {e}"));
    }
    let action = match hook::parse_input(&stdin, &config.slot, invocation) {
        Ok(Action::Ignore) => return Outcome::silent(),
        Ok(action) => action,
        Err(e) => return policy.failure(format!("{e:#}")),
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => return policy.failure(format!("could not start the async runtime: {e}")),
    };
    match action {
        Action::BeforeTool(request) => {
            match runtime.block_on(transport::before_tool(config.socket, request)) {
                Ok(admission) => hook::outcome_for(&admission),
                Err(e) => policy.failure(format!("{e:#}")),
            }
        }
        Action::AfterTool(report) => {
            match runtime.block_on(transport::after_tool(config.socket, report)) {
                Ok(()) => Outcome::silent(),
                Err(e) => policy.failure(format!("could not report the tool call: {e:#}")),
            }
        }
        Action::Ignore => Outcome::silent(),
    }
}
