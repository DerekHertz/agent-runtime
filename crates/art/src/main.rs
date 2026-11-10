//! `art`: the agent-runtime command line.
//!
//! `art run <run-file>` starts a run: a coordinator and one headless session per slot.
//! `art query <run-dir> <question> [arguments]` answers a causal question about a finished run.
//! Each subcommand lives in its own module and returns the text to print, so this file only
//! dispatches and reports errors.

mod query;
mod run;

use std::process::ExitCode;

const USAGE: &str =
    "usage: art run <run-file> [--run-id <id>] [--runs-dir <dir, default \"runs\">] [--leases]
       art query <run-dir> <question> [arguments]

questions:
  concurrent-writes              every pair of concurrent writes to one path by two slots
  denies                         every write refused because another slot held the lease
  happens-before <a> <b>         whether event a happens-before event b
  concurrent <a> <b>             whether neither event happens-before the other
  knew <slot> <event> <at>       whether the slot knew of event when it recorded its event at
  causal-past <event>            every event that happens-before event";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.split_first() {
        Some((command, rest)) if command == "query" => query::run(rest),
        Some((command, rest)) if command == "run" => run::run(rest),
        _ => Err(anyhow::anyhow!("unknown or missing command")),
    };
    match result {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("art: {e:#}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
