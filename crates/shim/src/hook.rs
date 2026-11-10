//! Pure translation between Claude Code's hook protocol and the coordinator protocol.
//!
//! Nothing here touches a socket, the environment, stdin or stdout, so all of it is unit-tested.
//! The shim forwards the raw tool input and never classifies it (SPEC section 7, "Path
//! extraction"), and shim-facing messages carry no clocks.

use anyhow::{anyhow, bail, Context, Result};
use runtime_proto::v1::after_tool_report::Result as ReportResult;
use runtime_proto::v1::{
    admission::Verdict, Admission, AfterToolReport, BeforeToolRequest, LifecycleReport,
    ToolFailure, ToolSuccess,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;

use crate::cli::{HookEvent, Invocation};
use crate::lifecycle;

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
    /// A PostToolUseFailure's raw `error`, which must be a JSON string. Kept raw, and `Some` for
    /// an explicit `null`, so a wrong type is refused rather than read as absent.
    #[serde(default, deserialize_with = "present")]
    error: Option<Box<RawValue>>,
    /// A PostToolUseFailure's raw `is_interrupt`, which may be omitted but, when present, must
    /// be `true` or `false`. Kept raw for the same reason as `error`.
    #[serde(default, deserialize_with = "present")]
    is_interrupt: Option<Box<RawValue>>,
    /// Stop's raw `stop_hook_active`, `last_assistant_message`, and SessionEnd's raw `reason`
    /// (see `lifecycle`). Kept raw for the same reason as `error`.
    #[serde(default, deserialize_with = "present")]
    stop_hook_active: Option<Box<RawValue>>,
    #[serde(default, deserialize_with = "present")]
    last_assistant_message: Option<Box<RawValue>>,
    #[serde(default, deserialize_with = "present")]
    reason: Option<Box<RawValue>>,
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
    /// Report this PostToolUse or PostToolUseFailure to the coordinator.
    AfterTool(AfterToolReport),
    /// Report this Stop or SessionEnd to the coordinator.
    Lifecycle(LifecycleReport),
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
    if matches!(event, HookEvent::Stop | HookEvent::SessionEnd) {
        let fields = lifecycle::Fields {
            stop_hook_active: input.stop_hook_active.as_deref(),
            last_assistant_message: input.last_assistant_message.as_deref(),
            reason: input.reason.as_deref(),
        };
        return lifecycle::report(event, slot, session_id, &fields).map(Action::Lifecycle);
    }
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
        HookEvent::PostToolUseFailure => {
            let failure = failure(name, input.error, input.is_interrupt)?;
            Ok(Action::AfterTool(AfterToolReport {
                slot: slot.to_owned(),
                session_id,
                tool,
                tool_input_json: tool_input.get().to_owned(),
                tool_use_id,
                result: Some(ReportResult::Failure(failure)),
            }))
        }
        // Returned above, before the tool fields are required.
        HookEvent::Stop | HookEvent::SessionEnd => bail!("{name} is not a tool event"),
    }
}

/// A PostToolUseFailure's result. `error` must be present and a JSON string; its raw text is
/// forwarded unchanged, so an empty message is the JSON `""` and every escape survives.
/// `is_interrupt` may be omitted, which is forwarded as unset, never as `false`; when present it
/// must be a boolean. Any other type, `null` included, is refused.
fn failure(
    name: &str,
    error: Option<Box<RawValue>>,
    is_interrupt: Option<Box<RawValue>>,
) -> Result<ToolFailure> {
    let error = error.ok_or_else(|| anyhow!("{name} input has no error"))?;
    // A raw JSON value starts at its first byte, and only a string starts with a quote.
    if !error.get().starts_with('"') {
        bail!("{name} input's error is not a string: {}", error.get());
    }
    let is_interrupt = match is_interrupt.as_deref().map(RawValue::get) {
        None => None,
        Some("true") => Some(true),
        Some("false") => Some(false),
        Some(other) => bail!("{name} input's is_interrupt is not a boolean: {other}"),
    };
    Ok(ToolFailure {
        error_json: error.get().to_owned(),
        is_interrupt,
    })
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
    const FAILURE: &str = include_str!("../tests/fixtures/post_tool_use_failure_bash.json");
    const STOP: &str = include_str!("../tests/fixtures/stop.json");
    const SESSION_END: &str = include_str!("../tests/fixtures/session_end.json");

    const LEGACY: Invocation = Invocation::Legacy;
    const PRE: Invocation = Invocation::Registered(HookEvent::PreToolUse);
    const REPORT: Invocation = Invocation::Registered(HookEvent::PostToolUse);
    const FAILED: Invocation = Invocation::Registered(HookEvent::PostToolUseFailure);

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
            other => panic!("expected a success, got {other:?}"),
        }
    }

    fn failed(stdin: &str) -> (AfterToolReport, ToolFailure) {
        match parse_input(stdin, "a", FAILED).unwrap() {
            Action::AfterTool(r) => match r.result.clone() {
                Some(ReportResult::Failure(f)) => (r, f),
                other => panic!("expected a failure result, got {other:?}"),
            },
            other => panic!("expected a failure report, got {other:?}"),
        }
    }

    /// The failure fixture with `"error":...,"is_interrupt":false` swapped by text for `fields`,
    /// so every other byte stays as recorded.
    fn failure_with(fields: &str) -> String {
        let start = FAILURE.find(r#""error":"#).unwrap();
        let end = FAILURE.find(r#","duration_ms""#).unwrap();
        format!("{}{}{}", &FAILURE[..start], fields, &FAILURE[end..])
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
        for invocation in [LEGACY, PRE, REPORT, FAILED] {
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
    fn post_tool_use_failure_reports_the_raw_input_error_and_flag() {
        let (r, f) = failed(FAILURE);
        let hook: Value = serde_json::from_str(FAILURE).unwrap();
        assert_eq!(r.slot, "a");
        assert_eq!(r.session_id, hook["session_id"]);
        assert_eq!(r.tool, "Bash");
        assert_eq!(r.tool_use_id, hook["tool_use_id"]);
        let want: Value = serde_json::from_str(&r.tool_input_json).unwrap();
        assert_eq!(want, hook["tool_input"]);
        assert!(FAILURE.contains(&format!(r#""tool_input":{}"#, r.tool_input_json)));
        assert!(FAILURE.contains(&format!(r#""error":{}"#, f.error_json)));
        assert!(
            f.error_json.starts_with(r#""Exit code 1\n"#),
            "{}",
            f.error_json
        );
        assert_eq!(f.is_interrupt, Some(false));
    }

    #[test]
    fn the_error_text_and_the_interrupt_flag_are_kept_as_reported() {
        for (fields, error, flag) in [
            (r#""error":"","is_interrupt":true"#, "\"\"", Some(true)),
            (
                r#""error": "a\u00e9\"b\n" ,"is_interrupt":false"#,
                r#""a\u00e9\"b\n""#,
                Some(false),
            ),
            (r#""error":"interrupted""#, r#""interrupted""#, None),
        ] {
            let stdin = failure_with(fields);
            let (r, f) = failed(&stdin);
            assert_eq!(
                (f.error_json.as_str(), f.is_interrupt),
                (error, flag),
                "{stdin}"
            );
            assert_eq!(r.tool, "Bash");
        }
    }

    #[test]
    fn a_failure_with_a_missing_or_mistyped_field_is_an_error() {
        for missing in [
            "session_id",
            "tool_name",
            "tool_input",
            "tool_use_id",
            "error",
        ] {
            let mut v: Value = serde_json::from_str(FAILURE).unwrap();
            v.as_object_mut().unwrap().remove(missing);
            let err = parse_input(&v.to_string(), "a", FAILED).unwrap_err();
            assert!(format!("{err:#}").contains(missing), "{err:#}");
        }
        for (fields, named) in [
            (r#""error":null"#, "error"),
            (r#""error":1"#, "error"),
            (r#""error":{"message":"x"}"#, "error"),
            (r#""error":["x"]"#, "error"),
            (r#""error":false"#, "error"),
            (r#""error":"x","is_interrupt":null"#, "is_interrupt"),
            (r#""error":"x","is_interrupt":"true""#, "is_interrupt"),
            (r#""error":"x","is_interrupt":1"#, "is_interrupt"),
        ] {
            let err = parse_input(&failure_with(fields), "a", FAILED).unwrap_err();
            assert!(format!("{err:#}").contains(named), "{fields}: {err:#}");
        }
        for field in ["session_id", "tool_name", "tool_use_id"] {
            let mut v: Value = serde_json::from_str(FAILURE).unwrap();
            v[field] = json!(7);
            assert!(parse_input(&v.to_string(), "a", FAILED).is_err(), "{field}");
        }
        let mut v: Value = serde_json::from_str(FAILURE).unwrap();
        v["tool_input"] = Value::Null;
        assert!(
            parse_input(&v.to_string(), "a", FAILED).is_err(),
            "null input"
        );
    }

    #[test]
    fn failure_and_success_registrations_take_only_their_own_event() {
        for (stdin, invocation) in [
            (FAILURE, REPORT),
            (POST, FAILED),
            (EDIT, FAILED),
            (FAILURE, PRE),
        ] {
            let err = parse_input(stdin, "a", invocation).unwrap_err();
            assert!(format!("{err:#}").contains("registered for"), "{err:#}");
        }
        assert_eq!(parse_input(FAILURE, "a", LEGACY).unwrap(), Action::Ignore);
        // A success ignores error fields, and a failure ignores a tool_response.
        let post = POST.replace(
            r#""tool_use_id""#,
            r#""error":7,"is_interrupt":"x","tool_use_id""#,
        );
        assert_eq!(
            response(&after_tool(&post)),
            r#"{"filePath":"/work/src/lib.rs"}"#
        );
        let with_response = FAILURE.replace(r#""error""#, r#""tool_response":5,"error""#);
        assert_eq!(failed(&with_response).1.is_interrupt, Some(false));
    }

    fn lifecycle_of(stdin: &str, event: HookEvent) -> Result<LifecycleReport> {
        match parse_input(stdin, "a", Invocation::Registered(event))? {
            Action::Lifecycle(r) => Ok(r),
            other => panic!("expected a lifecycle report, got {other:?}"),
        }
    }

    #[test]
    fn stop_and_session_end_report_their_observed_fields() {
        use runtime_proto::v1::LifecycleEvent;
        let stop = lifecycle_of(STOP, HookEvent::Stop).unwrap();
        assert_eq!(
            (stop.slot.as_str(), stop.session_id.as_str(), stop.event),
            (
                "a",
                "90c75de7-f344-4da3-9b24-152c5bfda374",
                LifecycleEvent::Stop as i32
            )
        );
        assert_eq!(
            stop.fields_json,
            r#"{"stop_hook_active":false,"last_assistant_message":"done"}"#
        );
        let end = lifecycle_of(SESSION_END, HookEvent::SessionEnd).unwrap();
        assert_eq!(end.event, LifecycleEvent::SessionEnd as i32);
        assert_eq!(end.fields_json, r#"{"reason":"other"}"#);
        // A lifecycle event needs no tool fields; an omitted lifecycle field stays absent.
        let bare = r#"{"hook_event_name":"Stop","session_id":"s"}"#;
        assert_eq!(
            lifecycle_of(bare, HookEvent::Stop).unwrap().fields_json,
            "{}"
        );
    }

    #[test]
    fn a_lifecycle_report_needs_its_session_and_well_typed_fields() {
        for (stdin, event) in [
            (STOP, HookEvent::Stop),
            (SESSION_END, HookEvent::SessionEnd),
        ] {
            let mut v: Value = serde_json::from_str(stdin).unwrap();
            v.as_object_mut().unwrap().remove("session_id");
            let err = lifecycle_of(&v.to_string(), event).unwrap_err();
            assert!(format!("{err:#}").contains("session_id"), "{err:#}");
        }
        for (stdin, event, field, value) in [
            (STOP, HookEvent::Stop, "stop_hook_active", json!(null)),
            (STOP, HookEvent::Stop, "stop_hook_active", json!("false")),
            (STOP, HookEvent::Stop, "last_assistant_message", json!(null)),
            (SESSION_END, HookEvent::SessionEnd, "reason", json!(null)),
            (SESSION_END, HookEvent::SessionEnd, "reason", json!(3)),
        ] {
            let mut v: Value = serde_json::from_str(stdin).unwrap();
            v[field] = value;
            let err = lifecycle_of(&v.to_string(), event).unwrap_err();
            assert!(format!("{err:#}").contains(field), "{err:#}");
        }
    }

    #[test]
    fn lifecycle_registrations_take_only_their_own_event() {
        for (stdin, event) in [
            (STOP, HookEvent::SessionEnd),
            (SESSION_END, HookEvent::Stop),
            (POST, HookEvent::Stop),
            (EDIT, HookEvent::SessionEnd),
            (STOP, HookEvent::PostToolUse),
            (SESSION_END, HookEvent::PostToolUseFailure),
            (STOP, HookEvent::PreToolUse),
        ] {
            let err = parse_input(stdin, "a", Invocation::Registered(event)).unwrap_err();
            assert!(format!("{err:#}").contains("registered for"), "{err:#}");
        }
        for stdin in [STOP, SESSION_END] {
            assert_eq!(parse_input(stdin, "a", LEGACY).unwrap(), Action::Ignore);
        }
    }

    #[test]
    fn every_gated_tool_forwards_its_raw_input() {
        for (stdin, tool) in [
            (EDIT, "Edit"),
            (BASH, "Bash"),
            (
                include_str!("../tests/fixtures/pre_tool_use_read.json"),
                "Read",
            ),
            (
                include_str!("../tests/fixtures/pre_tool_use_write_synthetic.json"),
                "Write",
            ),
            (
                include_str!("../tests/fixtures/pre_tool_use_notebook_edit_synthetic.json"),
                "NotebookEdit",
            ),
        ] {
            let r = before_tool(stdin);
            assert_eq!(r.tool, tool);
            assert!(
                stdin.contains(&format!(r#""tool_input":{}"#, r.tool_input_json)),
                "{tool}: {}",
                r.tool_input_json
            );
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
