//! Stop and SessionEnd hook input to a `LifecycleReport` (SPEC sections 3 and 5).
//!
//! A lifecycle report's payload is a JSON object holding exactly the lifecycle fields the hook
//! carried, each value its raw JSON text, in a fixed key order: for Stop `stop_hook_active` (a
//! boolean) then `last_assistant_message` (a string), for SessionEnd `reason` (a string). A field
//! the hook omitted is absent from the object, never filled in, so a hook that carried none gives
//! `{}`. A present field of another type, `null` included, is refused (exit 1), like a mistyped
//! PostToolUseFailure field. Other hook fields (`transcript_path`, `cwd`, ...) are not kept.

use anyhow::{bail, Result};
use runtime_proto::v1::{LifecycleEvent, LifecycleReport};
use serde_json::value::RawValue;

use crate::cli::HookEvent;

/// The raw lifecycle fields of a hook input, `Some` for any present value, `null` included.
#[derive(Debug, Default)]
pub struct Fields<'a> {
    pub stop_hook_active: Option<&'a RawValue>,
    pub last_assistant_message: Option<&'a RawValue>,
    pub reason: Option<&'a RawValue>,
}

#[derive(Clone, Copy)]
enum Type {
    Boolean,
    String,
}

impl Type {
    fn admits(self, raw: &str) -> bool {
        match self {
            Type::Boolean => raw == "true" || raw == "false",
            // A raw JSON value starts at its first byte, and only a string starts with a quote.
            Type::String => raw.starts_with('"'),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Type::Boolean => "a boolean",
            Type::String => "a string",
        }
    }
}

/// The report for a Stop or SessionEnd event of `session_id` on `slot`. Any other event is a bug
/// in the caller and is refused.
pub fn report(
    event: HookEvent,
    slot: &str,
    session_id: String,
    fields: &Fields,
) -> Result<LifecycleReport> {
    let (wire, kept) = match event {
        HookEvent::Stop => (
            LifecycleEvent::Stop,
            vec![
                ("stop_hook_active", fields.stop_hook_active, Type::Boolean),
                (
                    "last_assistant_message",
                    fields.last_assistant_message,
                    Type::String,
                ),
            ],
        ),
        HookEvent::SessionEnd => (
            LifecycleEvent::SessionEnd,
            vec![("reason", fields.reason, Type::String)],
        ),
        other => bail!("{} is not a lifecycle event", other.name()),
    };
    let mut members = Vec::new();
    for (key, raw, ty) in kept {
        let Some(raw) = raw.map(RawValue::get) else {
            continue;
        };
        if !ty.admits(raw) {
            bail!("{} input's {key} is not {}: {raw}", event.name(), ty.name());
        }
        members.push(format!("\"{key}\":{raw}"));
    }
    Ok(LifecycleReport {
        slot: slot.to_owned(),
        session_id,
        event: wire as i32,
        fields_json: format!("{{{}}}", members.join(",")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(text: &str) -> Box<RawValue> {
        RawValue::from_string(text.to_owned()).unwrap()
    }

    fn fields_of(event: HookEvent, values: &[(&str, &str)]) -> Result<String> {
        let owned: Vec<(String, Box<RawValue>)> = values
            .iter()
            .map(|(k, v)| (k.to_string(), raw(v)))
            .collect();
        let get = |name: &str| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_ref())
        };
        let fields = Fields {
            stop_hook_active: get("stop_hook_active"),
            last_assistant_message: get("last_assistant_message"),
            reason: get("reason"),
        };
        Ok(report(event, "a", "s".into(), &fields)?.fields_json)
    }

    #[test]
    fn stop_keeps_its_present_fields_raw_in_a_fixed_order() {
        let got = fields_of(
            HookEvent::Stop,
            &[
                ("last_assistant_message", r#""aé\"b\n""#),
                ("stop_hook_active", "true"),
            ],
        )
        .unwrap();
        assert_eq!(
            got,
            r#"{"stop_hook_active":true,"last_assistant_message":"aé\"b\n"}"#
        );
        let empty_message = fields_of(
            HookEvent::Stop,
            &[
                ("stop_hook_active", "false"),
                ("last_assistant_message", r#""""#),
            ],
        )
        .unwrap();
        assert_eq!(
            empty_message,
            r#"{"stop_hook_active":false,"last_assistant_message":""}"#
        );
    }

    #[test]
    fn omitted_fields_are_absent_never_filled_in() {
        assert_eq!(fields_of(HookEvent::Stop, &[]).unwrap(), "{}");
        assert_eq!(
            fields_of(HookEvent::Stop, &[("stop_hook_active", "false")]).unwrap(),
            r#"{"stop_hook_active":false}"#
        );
        assert_eq!(
            fields_of(HookEvent::Stop, &[("last_assistant_message", r#""x""#)]).unwrap(),
            r#"{"last_assistant_message":"x"}"#
        );
        assert_eq!(fields_of(HookEvent::SessionEnd, &[]).unwrap(), "{}");
    }

    #[test]
    fn each_event_keeps_only_its_own_fields() {
        assert_eq!(
            fields_of(
                HookEvent::SessionEnd,
                &[("reason", r#""clear""#), ("stop_hook_active", "7")]
            )
            .unwrap(),
            r#"{"reason":"clear"}"#
        );
        assert_eq!(
            fields_of(
                HookEvent::Stop,
                &[("reason", "7"), ("stop_hook_active", "true")]
            )
            .unwrap(),
            r#"{"stop_hook_active":true}"#
        );
    }

    #[test]
    fn a_mistyped_or_null_field_is_refused() {
        for (event, field, value) in [
            (HookEvent::Stop, "stop_hook_active", "null"),
            (HookEvent::Stop, "stop_hook_active", r#""false""#),
            (HookEvent::Stop, "stop_hook_active", "0"),
            (HookEvent::Stop, "last_assistant_message", "null"),
            (HookEvent::Stop, "last_assistant_message", "1"),
            (HookEvent::Stop, "last_assistant_message", r#"["x"]"#),
            (HookEvent::SessionEnd, "reason", "null"),
            (HookEvent::SessionEnd, "reason", "false"),
            (HookEvent::SessionEnd, "reason", r#"{"r":"x"}"#),
        ] {
            let err = fields_of(event, &[(field, value)]).unwrap_err();
            assert!(
                format!("{err:#}").contains(field),
                "{field}={value}: {err:#}"
            );
        }
    }

    #[test]
    fn the_event_maps_to_its_own_wire_kind() {
        for (event, wire) in [
            (HookEvent::Stop, LifecycleEvent::Stop),
            (HookEvent::SessionEnd, LifecycleEvent::SessionEnd),
        ] {
            let r = report(event, "slot-1", "sess".into(), &Fields::default()).unwrap();
            assert_eq!(
                (r.event, r.slot.as_str(), r.session_id.as_str()),
                (wire as i32, "slot-1", "sess")
            );
        }
        for event in [
            HookEvent::PreToolUse,
            HookEvent::PostToolUse,
            HookEvent::PostToolUseFailure,
        ] {
            assert!(report(event, "a", "s".into(), &Fields::default()).is_err());
        }
    }
}
