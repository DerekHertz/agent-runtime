//! `art query`: load a run and answer one causal question about it.
//!
//! All the logic is in `trace_store::Run`. This module parses the arguments and formats the
//! answer.

use std::fmt::Write;
use std::path::Path;

use anyhow::{bail, Context, Result};
use trace_store::Run;

/// Answer `art query <run-dir> <question> [arguments]`; `args` starts at the run directory.
pub fn run(args: &[String]) -> Result<String> {
    let [run_dir, question, rest @ ..] = args else {
        bail!("query needs a run directory and a question");
    };
    let run = Run::load(Path::new(run_dir))
        .with_context(|| format!("could not load the run in {run_dir}"))?;
    match (question.as_str(), rest) {
        ("concurrent-writes", []) => Ok(concurrent_writes(&run)),
        ("denies", []) => Ok(denies(&run)),
        ("happens-before", [a, b]) => Ok(line(run.happens_before(id(a)?, id(b)?)?)),
        ("concurrent", [a, b]) => Ok(line(run.concurrent(id(a)?, id(b)?)?)),
        ("knew", [slot, event, at]) => Ok(line(run.knew(slot, id(event)?, id(at)?)?)),
        ("causal-past", [event]) => Ok(run
            .causal_past(id(event)?)?
            .iter()
            .map(|e| format!("{e}\n"))
            .collect()),
        _ => bail!("unknown question or wrong number of arguments: {question}"),
    }
}

fn id(text: &str) -> Result<u64> {
    text.parse()
        .with_context(|| format!("{text:?} is not an event id"))
}

fn line(answer: bool) -> String {
    format!("{answer}\n")
}

/// One line per pair, `path<TAB>event (slot)<TAB>event (slot)`, then the count.
fn concurrent_writes(run: &Run) -> String {
    let pairs = run.concurrent_writes();
    let mut out = String::new();
    let slot = |event_id| {
        run.get(event_id)
            .map(|e| e.event.slot.as_str())
            .unwrap_or("?")
    };
    for pair in &pairs {
        let (a, b) = (pair.first, pair.second);
        let _ = writeln!(out, "{}\t{a} ({})\t{b} ({})", pair.path, slot(a), slot(b));
    }
    let _ = writeln!(out, "{} concurrent write pairs", pairs.len());
    out
}

/// One line per Deny, `event (slot)<TAB>path<TAB>reason`, then the count.
fn denies(run: &Run) -> String {
    let mut out = String::new();
    let mut count = 0;
    for deny in run.denies() {
        let e = &deny.event;
        let (path, reason) = (
            e.path.as_deref().unwrap_or("?"),
            deny.deny_reason.as_deref().unwrap_or("?"),
        );
        let _ = writeln!(out, "{} ({})\t{path}\t{reason}", e.event_id, e.slot);
        count += 1;
    }
    let _ = writeln!(out, "{count} denies");
    out
}
