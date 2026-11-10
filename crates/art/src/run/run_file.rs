//! The run file: what `art run` is asked to start.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// A run: one copy of a target tree, and one headless session per slot working in it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunFile {
    /// The directory copied into the run as the sessions' shared working tree. A relative path
    /// is taken from the run file's own directory.
    pub target: PathBuf,
    pub slots: Vec<SlotSpec>,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_allowed_tools")]
    pub allowed_tools: String,
    /// The program started once per slot. Anything but `claude` is a stand-in for tests.
    #[serde(default = "default_agent")]
    pub agent: String,
    /// The PreToolUse hook timeout: the longest a gate may hold a call, plus 30 s (SPEC
    /// section 3).
    #[serde(default = "default_gate_timeout")]
    pub gate_timeout_s: u64,
    #[serde(default = "default_session_timeout")]
    pub session_timeout_s: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotSpec {
    pub slot: String,
    pub prompt: String,
}

fn default_model() -> String {
    "haiku".into()
}

fn default_allowed_tools() -> String {
    "Read,Edit,Write,Bash(cargo test:*)".into()
}

fn default_agent() -> String {
    "claude".into()
}

fn default_gate_timeout() -> u64 {
    90
}

fn default_session_timeout() -> u64 {
    600
}

impl RunFile {
    pub fn read(path: &Path) -> Result<RunFile> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("could not read the run file {}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        RunFile::parse(&text, base)
            .with_context(|| format!("the run file {} is not valid", path.display()))
    }

    /// Parse run-file JSON; a relative `target` is resolved against `base`.
    pub fn parse(text: &str, base: &Path) -> Result<RunFile> {
        let mut file: RunFile = serde_json::from_str(text)?;
        if file.slots.is_empty() {
            bail!("a run needs at least one slot");
        }
        for (i, spec) in file.slots.iter().enumerate() {
            if spec.slot.is_empty() || spec.slot.contains([',', '/']) {
                bail!(
                    "slot name {:?} must be nonempty with no ',' or '/'",
                    spec.slot
                );
            }
            if file.slots[..i].iter().any(|s| s.slot == spec.slot) {
                bail!("slot {:?} is listed twice", spec.slot);
            }
        }
        if file.target.is_relative() {
            file.target = base.join(&file.target);
        }
        Ok(file)
    }

    pub fn slot_names(&self) -> Vec<&str> {
        self.slots.iter().map(|s| s.slot.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_run_file_gets_the_m0_defaults_and_a_resolved_target() {
        let text = r#"{"target":"toy","slots":[{"slot":"slot-1","prompt":"a"},{"slot":"slot-2","prompt":"b"}]}"#;
        let file = RunFile::parse(text, Path::new("/bench")).unwrap();
        assert_eq!(file.target, PathBuf::from("/bench/toy"));
        assert_eq!(file.slot_names(), ["slot-1", "slot-2"]);
        assert_eq!(
            (file.model.as_str(), file.agent.as_str()),
            ("haiku", "claude")
        );
        assert_eq!((file.gate_timeout_s, file.session_timeout_s), (90, 600));
    }

    #[test]
    fn bad_run_files_are_refused_with_the_reason() {
        let cases = [
            (r#"{"target":"t","slots":[]}"#, "at least one slot"),
            (
                r#"{"target":"t","slots":[{"slot":"a","prompt":"x"},{"slot":"a","prompt":"y"}]}"#,
                "listed twice",
            ),
            (
                r#"{"target":"t","slots":[{"slot":"a,b","prompt":"x"}]}"#,
                "must be nonempty",
            ),
            (
                r#"{"target":"t","slots":[{"slot":"a","prompt":"x"}],"modle":"x"}"#,
                "unknown field",
            ),
        ];
        for (text, want) in cases {
            let err = format!("{:#}", RunFile::parse(text, Path::new(".")).unwrap_err());
            assert!(err.contains(want), "{err}");
        }
    }
}
