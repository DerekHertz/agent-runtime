//! Pure translation between Claude Code's hook protocol and the coordinator protocol.
//!
//! Nothing here touches a socket, the environment, stdin or stdout, so all of it is unit-tested.
//! The shim forwards the raw tool input and never classifies it (SPEC section 7, "Path
//! extraction"), and shim-facing messages carry no clocks.

use anyhow::{anyhow, bail, Context, Result};
use runtime_proto::v1::after_tool_report::Result as ReportResult;
use runtime_proto::v1::{
    admission::Verdict, Admission, AfterToolReport, BeforeToolRequest, ToolSuccess,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;

use crate::cli::{HookEvent, Invocation};

/// Prefix on every message the shim writes to stderr, so the model can tell why a call was blocked.
const REASON_PREFIX: &str = "agent-runtime:";

/// The fields of the hook's stdin JSON that the shim reads. Everything else is ignored.
#[derive(Deserialize)]
struct HookInput {
    hook_event_name: String,
    session_id: Option<String>,
    tool_name: Option<String>,
    /// Kept as raw JSON so the coordinator receives the agent's input byte for byte. `None` when
    /// the field is absent; an explicit `null` is not a tool input and counts as absent.
    #[serde(default, deserialize_with = "present")]
    tool_input: Option<Box<RawValue>>,
    /// A PostToolUse's raw response. `None` only when the field is absent: an explicit `null` is
    /// a response like any other JSON value, so it is kept as the raw text `null`.
    #[serde(default, deserialize_with = "present")]
    tool_response: Option<Box<RawValue>>,
    tool_use_id: Option<String>,
}

/// `Some` for any present value, `null` included. A plain `Option<Box<RawValue>>` would read an
/// explicit `null` as `None`, losing a valid tool response; `#[serde(default)]` gives `None` only
/// for an absent field.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Box<RawValue>>, D::Error> {
    Box::<RawValue>::deserialize(d).map(Some)
}

/// What to do with a hook invocation.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Ask the coordinator about this PreToolUse.
    BeforeTool(BeforeToolRequest),
    /// Report this PostToolUse to the coordinator.
    AfterTool(AfterToolReport),
    /// An event the legacy registration (no `--hook-event`) does not handle: exit 0 and print
    /// nothing. That registration is a PreToolUse gate only, so other events pass through
    /// silently, as before reports existed.
    Ignore,
}

/// Parse the hook's stdin for the shim running as `slot`, invoked as `invocation`. A registered
/// event must match the stdin's `hook_event_name`.
pub fn parse_input(stdin: &str, slot: &str, invocation: Invocation) -> Result<Action> {
    let input: HookInput =
        serde_json::from_str(stdin).context("hook input is not a valid hook JSON object")?;
    let event = match invocation {
        Invocation::Legacy if input.hook_event_name == "PreToolUse" => HookEvent::PreToolUse,
        Invocation::Legacy => return Ok(Action::Ignore),
        Invocation::Registered(event) if event.name() == input.hook_event_name => event,
        Invocation::Registered(event) => bail!(
            "the shim is registered for {} but received a {:?} event",
            event.name(),
            input.hook_event_name
        ),
    };
    let name = event.name();
    let session_id = input
        .session_id
        .ok_or_else(|| anyhow!("{name} input has no session_id"))?;
    let tool = input
        .tool_name
        .ok_or_else(|| anyhow!("{name} input has no tool_name"))?;
    let tool_input = input
        .tool_input
        .filter(|raw| raw.get() != "null")
        .ok_or_else(|| anyhow!("{name} input has no tool_input"))?;
    let tool_use_id = input
        .tool_use_id
        .ok_or_else(|| anyhow!("{name} input has no tool_use_id"))?;
    match event {
        HookEvent::PreToolUse => {
            let intent = intent(&tool, &tool_input);
            Ok(Action::BeforeTool(BeforeToolRequest {
                slot: slot.to_owned(),
                session_id,
                tool,
                tool_input_json: tool_input.get().to_owned(),
                intent,
                tool_use_id,
            }))
        }
        HookEvent::PostToolUse => {
            let response = input
                .tool_response
                .ok_or_else(|| anyhow!("{name} input has no tool_response"))?;
            Ok(Action::AfterTool(AfterToolReport {
                slot: slot.to_owned(),
                session_id,
                tool,
                tool_input_json: tool_input.get().to_owned(),
                tool_use_id,
                result: Some(ReportResult::Success(ToolSuccess {
                    tool_response_json: response.get().to_owned(),
                })),
            }))
        }
    }
}

/// The agent's stated intent: a Bash call's `description` when it is a string, else empty.
fn intent(tool: &str, tool_input: &RawValue) -> String {
    if tool != "Bash" {
        return String::new();
    }
    #[derive(Deserialize)]
    struct Bash {
        description: Option<String>,
    }
    serde_json::from_str::<Bash>(tool_input.get())
        .ok()
        .and_then(|b| b.description)
        .unwrap_or_default()
}

/// What the shim finally does: print `stdout` (when present), write `stderr` (when present), and
/// exit with `code`.
#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub code: u8,
}

impl Outcome {
    /// Exit 0 with no output: Claude Code carries on as if no hook ran.
    pub fn silent() -> Self {
        Outcome {
            stdout: None,
            stderr: None,
            code: 0,
        }
    }

    /// Block the call. Exit 2 is the only failure code Claude Code treats as a block; every other
    /// non-zero code lets the call proceed, so every failure path of a gate must come here.
    pub fn fail_closed(reason: impl std::fmt::Display) -> Self {
        Outcome {
            stdout: None,
            stderr: Some(format!("{REASON_PREFIX} {reason}")),
            code: 2,
        }
    }

    /// A report that could not be made: exit 1, a nonblocking error that Claude Code shows in its
    /// transcript, with the reason on stderr and nothing on stdout. Never exit 2, which would feed
    /// the error to the model (PostToolUse) or keep a session from stopping (Stop).
    pub fn report_failed(reason: impl std::fmt::Display) -> Self {
        Outcome {
            stdout: None,
            stderr: Some(format!("{REASON_PREFIX} {reason}")),
            code: 1,
        }
    }
}

/// How the shim fails, fixed by its registration before anything can go wrong (see `cli`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// A PreToolUse gate: exit 2, which blocks the call.
    FailClosed,
    /// A report: exit 1, which blocks nothing.
    Nonblocking,
}

impl Policy {
    /// The exit code of a failure under this policy.
    pub fn code(self) -> u8 {
        match self {
            Policy::FailClosed => 2,
            Policy::Nonblocking => 1,
        }
    }

    pub fn failure(self, reason: impl std::fmt::Display) -> Outcome {
        match self {
            Policy::FailClosed => Outcome::fail_closed(reason),
            Policy::Nonblocking => Outcome::report_failed(reason),
        }
    }
}

#[derive(Serialize)]
struct Output<'a> {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: Specific<'a>,
}

#[derive(Serialize)]
struct Specific<'a> {
    #[serde(rename = "hookEventName")]
    hook_event_name: &'static str,
    #[serde(rename = "permissionDecision")]
    permission_decision: &'static str,
    #[serde(
        rename = "permissionDecisionReason",
        skip_serializing_if = "Option::is_none"
    )]
    permission_decision_reason: Option<&'a str>,
    #[serde(rename = "updatedInput", skip_serializing_if = "Option::is_none")]
    updated_input: Option<&'a RawValue>,
}

fn decision(decision: &'static str, reason: Option<&str>, updated: Option<&RawValue>) -> Outcome {
    let out = Output {
        hook_specific_output: Specific {
            hook_event_name: "PreToolUse",
            permission_decision: decision,
            permission_decision_reason: reason,
            updated_input: updated,
        },
    };
    match serde_json::to_string(&out) {
        Ok(json) => Outcome {
            stdout: Some(json),
            stderr: None,
            code: 0,
        },
        Err(e) => Outcome::fail_closed(format!("could not encode the decision: {e}")),
    }
}

/// Map the coordinator's admission to the hook output.
///
/// Allow and Deny print a decision object and exit 0. Defer is never produced in this slice, so
/// seeing it (or a verdict this build does not know) is a coordinator/shim mismatch and fails
/// closed rather than guessing.
pub fn outcome_for(admission: &Admission) -> Outcome {
    match map_admission(admission) {
        Ok(outcome) => outcome,
        Err(e) => Outcome::fail_closed(format!("{e:#}")),
    }
}

fn map_admission(admission: &Admission) -> Result<Outcome> {
    match Verdict::try_from(admission.verdict) {
        Ok(Verdict::Allow) => {
            let updated = if admission.updated_input_json.is_empty() {
                None
            } else {
                Some(
                    serde_json::from_str::<Box<RawValue>>(&admission.updated_input_json)
                        .context("the coordinator sent updated_input_json that is not JSON")?,
                )
            };
            Ok(decision("allow", None, updated.as_deref()))
        }
        Ok(Verdict::Deny) => {
            let reason = if admission.reason.is_empty() {
                "denied by the coordinator"
            } else {
                admission.reason.as_str()
            };
            Ok(decision("deny", Some(reason), None))
        }
        Ok(Verdict::Defer) => {
            bail!("the coordinator deferred the call, which this shim cannot wait for yet")
        }
        Err(_) => bail!(
            "the coordinator sent an unknown verdict ({})",
            admission.verdict
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const EDIT: &str = include_str!("../tests/fixtures/pre_tool_use_edit.json");
    const BASH: &str = include_str!("../tests/fixtures/pre_tool_use_bash.json");
    const POST: &str = include_str!("../tests/fixtures/post_tool_use_edit.json");

    const LEGACY: Invocation = Invocation::Legacy;
    const PRE: Invocation = Invocation::Registered(HookEvent::PreToolUse);
    const REPORT: Invocation = Invocation::Registered(HookEvent::PostToolUse);

    fn before_tool(stdin: &str) -> BeforeToolRequest {
        match parse_input(stdin, "a", LEGACY).unwrap() {
            Action::BeforeTool(r) => r,
            other => panic!("expected a BeforeTool action, got {other:?}"),
        }
    }

    fn after_tool(stdin: &str) -> AfterToolReport {
        match parse_input(stdin, "a", REPORT).unwrap() {
            Action::AfterTool(r) => r,
            other => panic!("expected an AfterTool action, got {other:?}"),
        }
    }

    fn response(report: &AfterToolReport) -> &str {
        match &report.result {
            Some(ReportResult::Success(s)) => &s.tool_response_json,
            None => panic!("a report always carries its result"),
        }
    }

    fn post(tool_response: &str) -> String {
        format!(
            r#"{{"hook_event_name":"PostToolUse","session_id":"s","tool_name":"Edit","tool_input":{{"z": 1,  "a":[1, 2]}},"tool_response": {tool_response} ,"tool_use_id":"t"}}"#
        )
    }

    fn json_of(outcome: &Outcome) -> Value {
        serde_json::from_str(outcome.stdout.as_deref().unwrap()).unwrap()
    }

    #[test]
    fn edit_forwards_the_raw_tool_input() {
        let r = before_tool(EDIT);
        assert_eq!(r.slot, "a");
        assert_eq!(r.session_id, "90c75de7-f344-4da3-9b24-152c5bfda374");
        assert_eq!(r.tool, "Edit");
        assert_eq!(r.intent, "");
        assert_eq!(r.tool_use_id, "toolu_01Uy28i9ZxHpdQkCRWhbwK3g");
        let want: Value = serde_json::from_str(EDIT).unwrap();
        let got: Value = serde_json::from_str(&r.tool_input_json).unwrap();
        assert_eq!(got, want["tool_input"]);
    }

    #[test]
    fn tool_input_is_forwarded_verbatim_including_key_order_and_spacing() {
        let stdin = r#"{"hook_event_name":"PreToolUse","session_id":"s","tool_name":"Write","tool_input":{"z": 1,  "a":[1, 2]},"tool_use_id":"t"}"#;
        assert_eq!(
            before_tool(stdin).tool_input_json,
            r#"{"z": 1,  "a":[1, 2]}"#
        );
    }

    #[test]
    fn bash_description_becomes_the_intent() {
        let r = before_tool(BASH);
        assert_eq!(r.tool, "Bash");
        assert_eq!(
            r.intent,
            "Read the stop hook script and list the run directory"
        );
    }

    #[test]
    fn bash_without_a_string_description_has_empty_intent() {
        for input in [
            json!({"command": "ls"}),
            json!({"command": "ls", "description": 3}),
        ] {
            let stdin = json!({"hook_event_name":"PreToolUse","session_id":"s","tool_name":"Bash",
                "tool_input":input,"tool_use_id":"t"});
            assert_eq!(before_tool(&stdin.to_string()).intent, "");
        }
    }

    #[test]
    fn only_bash_has_an_intent() {
        let stdin = json!({"hook_event_name":"PreToolUse","session_id":"s","tool_name":"Edit",
            "tool_input":{"file_path":"/x","description":"not an intent"},"tool_use_id":"t"});
        assert_eq!(before_tool(&stdin.to_string()).intent, "");
    }

    #[test]
    fn the_legacy_registration_ignores_other_events() {
        assert_eq!(parse_input(POST, "a", LEGACY).unwrap(), Action::Ignore);
        let stop = r#"{"hook_event_name":"Stop","session_id":"s"}"#;
        assert_eq!(parse_input(stop, "a", LEGACY).unwrap(), Action::Ignore);
    }

    #[test]
    fn a_registered_event_must_match_the_input() {
        match parse_input(EDIT, "a", PRE).unwrap() {
            Action::BeforeTool(r) => assert_eq!(r, before_tool(EDIT)),
            other => panic!("{other:?}"),
        }
        for (stdin, invocation) in [(POST, PRE), (EDIT, REPORT)] {
            let err = parse_input(stdin, "a", invocation).unwrap_err();
            assert!(format!("{err:#}").contains("registered for"), "{err:#}");
        }
        let stop = r#"{"hook_event_name":"Stop","session_id":"s"}"#;
        assert!(parse_input(stop, "a", REPORT).is_err());
    }

    #[test]
    fn malformed_input_is_an_error() {
        for invocation in [LEGACY, PRE, REPORT] {
            for bad in ["", "not json", "[]", r#"{"session_id":"s"}"#] {
                assert!(parse_input(bad, "a", invocation).is_err(), "{bad:?}");
            }
        }
    }

    #[test]
    fn pre_tool_use_missing_fields_is_an_error() {
        for missing in ["session_id", "tool_name", "tool_input", "tool_use_id"] {
            let mut v: Value = serde_json::from_str(EDIT).unwrap();
            v.as_object_mut().unwrap().remove(missing);
            assert!(
                parse_input(&v.to_string(), "a", LEGACY).is_err(),
                "{missing}"
            );
        }
        let mut v: Value = serde_json::from_str(EDIT).unwrap();
        v["tool_input"] = Value::Null;
        assert!(
            parse_input(&v.to_string(), "a", LEGACY).is_err(),
            "null input"
        );
    }

    #[test]
    fn post_tool_use_reports_the_raw_input_and_response() {
        let r = after_tool(POST);
        let hook: Value = serde_json::from_str(POST).unwrap();
        assert_eq!(r.slot, "a");
        assert_eq!(r.session_id, hook["session_id"]);
        assert_eq!(r.tool, "Edit");
        assert_eq!(r.tool_use_id, hook["tool_use_id"]);
        assert_eq!(
            r.tool_input_json,
            r#"{"file_path":"/work/src/lib.rs","old_string":"a","new_string":"b","replace_all":false}"#
        );
        assert_eq!(response(&r), r#"{"filePath":"/work/src/lib.rs"}"#);
    }

    #[test]
    fn every_json_response_is_kept_verbatim_including_null() {
        for raw in [
            "null",
            "0",
            "-1.5e3",
            r#""text""#,
            "true",
            "[ ]",
            r#"{"b": 1,  "a":null}"#,
        ] {
            let r = after_tool(&post(raw));
            assert_eq!(response(&r), raw);
            assert_eq!(r.tool_input_json, r#"{"z": 1,  "a":[1, 2]}"#);
        }
    }

    #[test]
    fn post_tool_use_missing_fields_is_an_error() {
        for missing in [
            "session_id",
            "tool_name",
            "tool_input",
            "tool_response",
            "tool_use_id",
        ] {
            let mut v: Value = serde_json::from_str(POST).unwrap();
            v.as_object_mut().unwrap().remove(missing);
            let err = parse_input(&v.to_string(), "a", REPORT).unwrap_err();
            assert!(format!("{err:#}").contains(missing), "{err:#}");
        }
    }

    #[test]
    fn policies_pick_the_exit_code() {
        let gate = Policy::FailClosed.failure("x");
        let report = Policy::Nonblocking.failure("x");
        assert_eq!((gate.code, report.code), (2, 1));
        for o in [gate, report] {
            assert_eq!(o.stdout, None);
            assert_eq!(o.stderr.as_deref(), Some("agent-runtime: x"));
        }
    }

    #[test]
    fn allow_prints_the_allow_object() {
        let o = outcome_for(&Admission {
            verdict: Verdict::Allow as i32,
            ..Default::default()
        });
        assert_eq!(o.code, 0);
        assert_eq!(o.stderr, None);
        assert_eq!(
            o.stdout.as_deref().unwrap(),
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}"#
        );
    }

    #[test]
    fn allow_carries_updated_input() {
        let o = outcome_for(&Admission {
            verdict: Verdict::Allow as i32,
            updated_input_json: r#"{"file_path":"/work/.art/snapshots/lib.rs"}"#.into(),
            ..Default::default()
        });
        assert_eq!(o.code, 0);
        assert_eq!(
            json_of(&o)["hookSpecificOutput"]["updatedInput"],
            json!({"file_path": "/work/.art/snapshots/lib.rs"})
        );
    }

    #[test]
    fn allow_with_invalid_updated_input_fails_closed() {
        let o = outcome_for(&Admission {
            verdict: Verdict::Allow as i32,
            updated_input_json: "{not json".into(),
            ..Default::default()
        });
        assert_eq!(o.code, 2);
        assert_eq!(o.stdout, None);
        assert!(o.stderr.unwrap().starts_with("agent-runtime:"));
    }

    #[test]
    fn deny_carries_the_reason() {
        let o = outcome_for(&Admission {
            verdict: Verdict::Deny as i32,
            reason: "b holds the lease on src/lib.rs".into(),
            owner: "b".into(),
            ..Default::default()
        });
        assert_eq!(o.code, 0);
        assert_eq!(
            json_of(&o),
            json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": "b holds the lease on src/lib.rs"
            }})
        );
    }

    #[test]
    fn deny_without_a_reason_still_explains_itself() {
        let o = outcome_for(&Admission {
            verdict: Verdict::Deny as i32,
            ..Default::default()
        });
        assert_eq!(o.code, 0);
        assert!(
            !json_of(&o)["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn defer_and_unknown_verdicts_fail_closed() {
        for verdict in [Verdict::Defer as i32, 99] {
            let o = outcome_for(&Admission {
                verdict,
                ..Default::default()
            });
            assert_eq!(o.code, 2);
            assert_eq!(o.stdout, None);
            assert!(o.stderr.unwrap().starts_with("agent-runtime:"));
        }
    }
}
