//! One headless agent session of a run: its command line, its clean environment, its output
//! files and its transcript.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

/// The parent variables a session keeps. Everything else is dropped, so a session signs in with
/// the logged-in subscription and sees no API key and no variable of a parent Claude Code
/// session (M0, `bench/m0/run_m0.py`).
const KEPT_ENV: [&str; 7] = ["HOME", "PATH", "USER", "LOGNAME", "SHELL", "LANG", "TMPDIR"];

/// What every session of a run shares.
#[derive(Debug)]
pub struct Launch<'a> {
    pub agent: &'a str,
    pub model: &'a str,
    pub allowed_tools: &'a str,
    pub settings: &'a Path,
    pub socket: &'a Path,
    pub work_dir: &'a Path,
}

/// A started session and where its output goes.
#[derive(Debug)]
pub struct Session {
    pub slot: String,
    pub session_id: String,
    pub dir: PathBuf,
    pub child: Child,
}

/// The session's environment: the kept parent variables, then the two that put it in the run.
pub fn clean_env(
    parent: impl Iterator<Item = (String, String)>,
    slot: &str,
    socket: &Path,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = parent
        .filter(|(name, _)| KEPT_ENV.contains(&name.as_str()))
        .collect();
    env.push(("AGENT_RUNTIME_SLOT".into(), slot.to_owned()));
    env.push((
        "AGENT_RUNTIME_SOCKET".into(),
        socket.to_string_lossy().into_owned(),
    ));
    env
}

/// The agent's arguments, as M0 proved them: headless, never `--bare`, a fixed session id.
pub fn agent_args(launch: &Launch, prompt: &str, session_id: &str) -> Vec<String> {
    let settings = launch.settings.to_string_lossy();
    [
        "-p",
        prompt,
        "--model",
        launch.model,
        "--settings",
        &settings,
        "--session-id",
        session_id,
        "--permission-mode",
        "acceptEdits",
        "--allowedTools",
        launch.allowed_tools,
        "--output-format",
        "json",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Start the session for `slot`. Its stdout and stderr go to `dir/stdout.json` and
/// `dir/stderr.txt`.
pub fn start(launch: &Launch, slot: &str, prompt: &str, dir: PathBuf) -> Result<Session> {
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let session_id = new_session_id()?;
    let child = Command::new(launch.agent)
        .args(agent_args(launch, prompt, &session_id))
        .current_dir(launch.work_dir)
        .env_clear()
        .envs(clean_env(std::env::vars(), slot, launch.socket))
        .stdin(Stdio::null())
        .stdout(File::create(dir.join("stdout.json"))?)
        .stderr(File::create(dir.join("stderr.txt"))?)
        .spawn()
        .with_context(|| format!("could not start {:?} for slot {slot}", launch.agent))?;
    Ok(Session {
        slot: slot.to_owned(),
        session_id,
        dir,
        child,
    })
}

/// Where Claude Code keeps the transcript of `session_id` started in `work_dir`: under
/// `~/.claude/projects/`, in a folder named by the working directory with every character
/// other than a letter, a digit or `-` turned into `-`.
pub fn transcript_path(home: &Path, work_dir: &Path, session_id: &str) -> PathBuf {
    let slug: String = work_dir
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    home.join(".claude/projects")
        .join(slug)
        .join(format!("{session_id}.jsonl"))
}

/// A random version 4 UUID, which `--session-id` requires.
fn new_session_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .context("could not read /dev/urandom for a session id")?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_keeps_no_api_key_and_no_parent_claude_code_variable() {
        let parent = [
            ("HOME", "/home/u"),
            ("PATH", "/bin"),
            ("ANTHROPIC_API_KEY", "sk-secret"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_ENTRYPOINT", "cli"),
            ("AGENT_RUNTIME_SLOT", "stale"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()));
        let env = clean_env(parent.into_iter(), "slot-2", Path::new("/tmp/c.sock"));
        let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(
            pairs,
            [
                ("HOME", "/home/u"),
                ("PATH", "/bin"),
                ("AGENT_RUNTIME_SLOT", "slot-2"),
                ("AGENT_RUNTIME_SOCKET", "/tmp/c.sock"),
            ]
        );
    }

    #[test]
    fn the_agent_is_started_headless_with_the_settings_and_a_fixed_session_id() {
        let launch = Launch {
            agent: "claude",
            model: "haiku",
            allowed_tools: "Read,Edit",
            settings: Path::new("/run/settings.json"),
            socket: Path::new("/tmp/c.sock"),
            work_dir: Path::new("/run/work"),
        };
        let args = agent_args(&launch, "fix it", "id-1");
        assert_eq!(&args[..2], ["-p", "fix it"]);
        for pair in [
            ["--settings", "/run/settings.json"],
            ["--session-id", "id-1"],
            ["--model", "haiku"],
            ["--allowedTools", "Read,Edit"],
        ] {
            assert!(args.windows(2).any(|w| w == pair), "{pair:?}");
        }
        assert!(!args.iter().any(|a| a == "--bare"));
    }

    #[test]
    fn session_ids_are_distinct_version_4_uuids() {
        let (a, b) = (new_session_id().unwrap(), new_session_id().unwrap());
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'4');
        assert!(matches!(a.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
    }

    #[test]
    fn the_transcript_folder_is_the_working_directory_with_separators_as_dashes() {
        assert_eq!(
            transcript_path(Path::new("/home/u"), Path::new("/tmp/runs/r_1/work"), "abc"),
            PathBuf::from("/home/u/.claude/projects/-tmp-runs-r-1-work/abc.jsonl")
        );
    }
}
