//! What a proposed tool call touches, read from its raw tool input (SPEC section 7).
//!
//! One definition for both users: the coordinator classifies a call when it admits it, and a
//! loaded run classifies the stored input of a trace written before the `path` column was set.
//! Pure: it reads only its arguments. Bash is not classified yet, so a write made through Bash
//! is `Other`.

/// The file a call reads or writes, when its tool names exactly one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// Edit, Write or NotebookEdit of `path`.
    Write { path: String },
    /// Read of `path`.
    Read { path: String },
    /// Any other tool, or one of the above whose input names no file.
    Other,
}

/// Each classified tool, the input field naming its file, and whether it writes.
const TOOLS: [(&str, &str, bool); 4] = [
    ("Edit", "file_path", true),
    ("Write", "file_path", true),
    ("NotebookEdit", "notebook_path", true),
    ("Read", "file_path", false),
];

impl Access {
    /// Classify a call of `tool` with the raw hook `tool_input`.
    pub fn classify(tool: &str, tool_input_json: &[u8]) -> Access {
        let Some((_, field, writes)) = TOOLS.iter().find(|(name, ..)| *name == tool) else {
            return Access::Other;
        };
        let path = serde_json::from_slice::<serde_json::Value>(tool_input_json)
            .ok()
            .and_then(|input| Some(input.get(field)?.as_str()?.to_owned()))
            .filter(|path| !path.is_empty());
        match (path, writes) {
            (Some(path), true) => Access::Write { path },
            (Some(path), false) => Access::Read { path },
            (None, _) => Access::Other,
        }
    }

    /// Whether `tool` is one whose call writes a file.
    pub fn is_write_tool(tool: &str) -> bool {
        TOOLS
            .iter()
            .any(|(name, _, writes)| *name == tool && *writes)
    }

    /// The file this access names, if any.
    pub fn path(&self) -> Option<&str> {
        match self {
            Access::Write { path } | Access::Read { path } => Some(path),
            Access::Other => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_file_tool_is_classified_by_its_own_path_field() {
        let write = |p: &str| Access::Write { path: p.into() };
        let cases = [
            (
                "Edit",
                r#"{"file_path":"/w/a.rs","old_string":"x"}"#,
                write("/w/a.rs"),
            ),
            (
                "Write",
                r#"{"file_path":"/w/b.rs","content":""}"#,
                write("/w/b.rs"),
            ),
            (
                "NotebookEdit",
                r#"{"notebook_path":"/w/n.ipynb"}"#,
                write("/w/n.ipynb"),
            ),
            (
                "Read",
                r#"{"file_path":"/w/a.rs"}"#,
                Access::Read {
                    path: "/w/a.rs".into(),
                },
            ),
            ("Bash", r#"{"command":"echo hi > /w/a.rs"}"#, Access::Other),
            ("Edit", r#"{"notebook_path":"/w/a.rs"}"#, Access::Other),
            ("Edit", r#"{"file_path":7}"#, Access::Other),
            ("Edit", r#"{"file_path":""}"#, Access::Other),
            ("Edit", "not json", Access::Other),
        ];
        for (tool, input, want) in cases {
            assert_eq!(
                Access::classify(tool, input.as_bytes()),
                want,
                "{tool} {input}"
            );
        }
        assert!(Access::is_write_tool("NotebookEdit") && !Access::is_write_tool("Read"));
    }
}
