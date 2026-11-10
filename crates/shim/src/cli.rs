//! The shim's command line: which hook registration invoked it.
//!
//! The command line comes from the hook registration in Claude Code's settings, which the
//! launcher writes, so it is trusted in a way stdin is not: it fixes how the shim fails before
//! the shim reads its configuration or stdin, or can panic. A PreToolUse gate fails closed
//! (exit 2), because Claude Code lets a tool call through on any other failure. A report
//! (PostToolUse) fails with a nonblocking exit 1, because the tool has already run and exit 2
//! would only feed an error back to the model, or for Stop, keep the session from stopping.
//!
//! - `art-shim`: the issue #2 registration, a PreToolUse gate. Other events pass through.
//! - `art-shim --hook-event PreToolUse`: a PreToolUse gate. Any other event is an error.
//! - `art-shim --hook-event PostToolUse`: a PostToolUse report. Any other event is an error.
//!
//! Once `--hook-event` names a report event (PostToolUse, or one a later build handles), every
//! command-line error is nonblocking, trailing arguments included.

use std::ffi::OsString;

use crate::hook::Policy;

/// A hook event the shim can be registered for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
}

impl HookEvent {
    /// The `hook_event_name` Claude Code sends for this event.
    pub fn name(self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
        }
    }

    pub fn policy(self) -> Policy {
        match self {
            HookEvent::PreToolUse => Policy::FailClosed,
            HookEvent::PostToolUse => Policy::Nonblocking,
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

/// Report events later issue #3 PRs handle. Registering one with this build is an error, but a
/// nonblocking one: a report must never block, whichever build runs it.
const LATER_REPORT_EVENTS: [&str; 3] = ["PostToolUseFailure", "Stop", "SessionEnd"];

/// What the value after `--hook-event` names, read before anything else is checked.
enum Hint<'a> {
    Known(HookEvent),
    LaterReport(&'a str),
    Unknown,
}

impl Hint<'_> {
    fn of(value: &OsString) -> Hint<'_> {
        match value.to_str() {
            Some("PreToolUse") => Hint::Known(HookEvent::PreToolUse),
            Some("PostToolUse") => Hint::Known(HookEvent::PostToolUse),
            Some(later) if LATER_REPORT_EVENTS.contains(&later) => Hint::LaterReport(later),
            _ => Hint::Unknown,
        }
    }

    /// How a command line with this hint fails. Only a recognized report event is nonblocking.
    fn policy(&self) -> Policy {
        match self {
            Hint::Known(event) => event.policy(),
            Hint::LaterReport(_) => Policy::Nonblocking,
            Hint::Unknown => Policy::FailClosed,
        }
    }
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
    let hint = Hint::of(value);
    if !extra.is_empty() {
        return Err(BadInvocation {
            reason: format!("unexpected arguments after --hook-event {value:?}: {extra:?}"),
            policy: hint.policy(),
        });
    }
    match hint {
        Hint::Known(event) => Ok(Invocation::Registered(event)),
        Hint::LaterReport(later) => Err(BadInvocation {
            reason: format!("this art-shim does not report {later} hooks yet"),
            policy: Policy::Nonblocking,
        }),
        Hint::Unknown => Err(fail_closed(format!("unknown hook event {value:?}"))),
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
    }

    #[test]
    fn a_later_report_event_fails_without_blocking() {
        for event in LATER_REPORT_EVENTS {
            let err = parse_strs(&["--hook-event", event]).unwrap_err();
            assert_eq!(err.policy, Policy::Nonblocking, "{event}");
            assert!(err.reason.contains(event), "{}", err.reason);
        }
    }

    #[test]
    fn trailing_arguments_after_a_report_event_fail_without_blocking() {
        let reports = ["PostToolUse"].into_iter().chain(LATER_REPORT_EVENTS);
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
