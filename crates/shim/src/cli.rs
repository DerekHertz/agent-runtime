//! The shim's command line: which hook registration invoked it.
//!
//! The command line comes from the hook registration in Claude Code's settings, which the
//! launcher writes, so it is trusted in a way stdin is not: it fixes how the shim fails before
//! the shim reads its configuration or stdin, or can panic. A PreToolUse gate fails closed
//! (exit 2), because Claude Code lets a tool call through on any other failure. A report
//! (PostToolUse, PostToolUseFailure, Stop, SessionEnd) fails with a nonblocking exit 1: the tool
//! has already run, so exit 2 would only feed an error back to the model, and on Stop it would
//! keep the session from stopping.
//!
//! - `art-shim`: the issue #2 registration, a PreToolUse gate. Other events pass through.
//! - `art-shim --hook-event PreToolUse`: a PreToolUse gate. Any other event is an error.
//! - `art-shim --hook-event PostToolUse`: a PostToolUse report. Any other event is an error.
//! - `art-shim --hook-event PostToolUseFailure`: a failed or interrupted call's report. Any other
//!   event is an error.
//! - `art-shim --hook-event Stop`, `art-shim --hook-event SessionEnd`: lifecycle reports. Any
//!   other event is an error.
//!
//! Once `--hook-event` names a report event, every command-line error is nonblocking, trailing
//! arguments included.

use std::ffi::OsString;

use crate::hook::Policy;

/// A hook event the shim can be registered for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    Stop,
    SessionEnd,
}

impl HookEvent {
    /// Every event, for parsing the command line.
    const ALL: [HookEvent; 5] = [
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::PostToolUseFailure,
        HookEvent::Stop,
        HookEvent::SessionEnd,
    ];

    /// The `hook_event_name` Claude Code sends for this event.
    pub fn name(self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::PostToolUseFailure => "PostToolUseFailure",
            HookEvent::Stop => "Stop",
            HookEvent::SessionEnd => "SessionEnd",
        }
    }

    /// Only the PreToolUse gate blocks. Every report, Stop included, is nonblocking: exit 2 on
    /// Stop would keep the session from stopping.
    pub fn policy(self) -> Policy {
        match self {
            HookEvent::PreToolUse => Policy::FailClosed,
            HookEvent::PostToolUse
            | HookEvent::PostToolUseFailure
            | HookEvent::Stop
            | HookEvent::SessionEnd => Policy::Nonblocking,
        }
    }
}

/// How the shim was invoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invocation {
    /// No arguments: a PreToolUse gate that lets every other event pass silently.
    Legacy,
    /// `--hook-event <event>`: the stdin JSON must be that event.
    Registered(HookEvent),
}

impl Invocation {
    pub fn policy(self) -> Policy {
        match self {
            Invocation::Legacy => Policy::FailClosed,
            Invocation::Registered(event) => event.policy(),
        }
    }
}

/// A command line the shim cannot honour, with the policy its failure must follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadInvocation {
    pub reason: String,
    pub policy: Policy,
}

/// What the value after `--hook-event` names, read before anything else is checked: an event
/// this build handles (exact, case-sensitive name), or `None`.
fn hint(value: &OsString) -> Option<HookEvent> {
    let value = value.to_str()?;
    HookEvent::ALL
        .into_iter()
        .find(|event| event.name() == value)
}

/// Parse the arguments after the program name.
///
/// A command line that starts `--hook-event <event>` with an event this build recognizes takes
/// that event's policy for every error, trailing arguments included, so a report registration
/// never blocks because of a stray argument after its event. Anything else fails closed, because
/// it may be a gate. The hint is trusted: a registration that names the wrong event gets that
/// event's policy, and the shim cannot tell before it reads stdin.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Invocation, BadInvocation> {
    let args: Vec<OsString> = args.into_iter().collect();
    let fail_closed = |reason: String| BadInvocation {
        reason,
        policy: Policy::FailClosed,
    };
    let Some((flag, rest)) = args.split_first() else {
        return Ok(Invocation::Legacy);
    };
    if flag != "--hook-event" {
        return Err(fail_closed(format!(
            "unknown argument {flag:?}; usage: art-shim [--hook-event <event>]"
        )));
    }
    let Some((value, extra)) = rest.split_first() else {
        return Err(fail_closed("--hook-event needs an event".into()));
    };
    let hint = hint(value);
    if !extra.is_empty() {
        return Err(BadInvocation {
            reason: format!("unexpected arguments after --hook-event {value:?}: {extra:?}"),
            // Only a recognized report event is nonblocking; anything else may be a gate.
            policy: hint.map_or(Policy::FailClosed, HookEvent::policy),
        });
    }
    match hint {
        Some(event) => Ok(Invocation::Registered(event)),
        None => Err(fail_closed(format!("unknown hook event {value:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Invocation, BadInvocation> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn no_arguments_is_the_legacy_gate() {
        assert_eq!(parse_strs(&[]), Ok(Invocation::Legacy));
        assert_eq!(Invocation::Legacy.policy(), Policy::FailClosed);
    }

    #[test]
    fn a_named_event_sets_its_policy() {
        let pre = parse_strs(&["--hook-event", "PreToolUse"]).unwrap();
        assert_eq!(pre, Invocation::Registered(HookEvent::PreToolUse));
        assert_eq!(pre.policy(), Policy::FailClosed);
        let post = parse_strs(&["--hook-event", "PostToolUse"]).unwrap();
        assert_eq!(post, Invocation::Registered(HookEvent::PostToolUse));
        assert_eq!(post.policy(), Policy::Nonblocking);
        let failure = parse_strs(&["--hook-event", "PostToolUseFailure"]).unwrap();
        assert_eq!(
            failure,
            Invocation::Registered(HookEvent::PostToolUseFailure)
        );
        assert_eq!(failure.policy(), Policy::Nonblocking);
    }

    #[test]
    fn stop_and_session_end_are_nonblocking_reports() {
        for (name, event) in [
            ("Stop", HookEvent::Stop),
            ("SessionEnd", HookEvent::SessionEnd),
        ] {
            let parsed = parse_strs(&["--hook-event", name]).unwrap();
            assert_eq!(parsed, Invocation::Registered(event));
            assert_eq!(parsed.policy(), Policy::Nonblocking, "{name}");
            assert_eq!(event.name(), name);
        }
    }

    #[test]
    fn every_event_parses_back_from_its_own_name() {
        for event in HookEvent::ALL {
            let parsed = parse_strs(&["--hook-event", event.name()]).unwrap();
            assert_eq!(parsed, Invocation::Registered(event));
        }
    }

    #[test]
    fn trailing_arguments_after_a_report_event_fail_without_blocking() {
        let reports = ["PostToolUse", "PostToolUseFailure", "Stop", "SessionEnd"];
        for event in reports {
            for extra in [&["extra"][..], &["--hook-event", "PreToolUse"], &[""]] {
                let mut args = vec!["--hook-event", event];
                args.extend_from_slice(extra);
                let err = parse_strs(&args).unwrap_err();
                assert_eq!(err.policy, Policy::Nonblocking, "{args:?}");
                assert!(err.reason.contains("unexpected"), "{}", err.reason);
            }
        }
    }

    #[test]
    fn trailing_arguments_after_a_gate_or_an_unknown_event_fail_closed() {
        for args in [
            &["--hook-event", "PreToolUse", "extra"][..],
            &["--hook-event", "PreToolUse", "--hook-event", "PostToolUse"],
            &["--hook-event", "Bogus", "extra"],
            &["--hook-event", "posttooluse", "extra"],
            &["--hook-event", "PostToolUseFailures", "extra"],
            &["--hook-event", "posttoolusefailure", "extra"],
            &["--hook-event", "stop", "extra"],
            &["--hook-event", "Session_End", "extra"],
        ] {
            let err = parse_strs(args).unwrap_err();
            assert_eq!(err.policy, Policy::FailClosed, "{args:?}");
        }
    }

    #[test]
    fn anything_else_fails_closed() {
        for args in [
            &["--hook-event"][..],
            &["--hook-event", "posttooluse"],
            &["--hook-event", "stop"],
            &["--hook-event", "sessionend"],
            &["--hook-event", "SubagentStop"],
            &["--hook-event", ""],
            &["--event", "PostToolUse"],
            &["--event", "PostToolUse", "extra"],
            &["PostToolUse"],
            &["--hook-event=PostToolUse"],
            &["--hook-event=PostToolUse", "extra"],
        ] {
            let err = parse_strs(args).unwrap_err();
            assert_eq!(err.policy, Policy::FailClosed, "{args:?}");
        }
    }
}
