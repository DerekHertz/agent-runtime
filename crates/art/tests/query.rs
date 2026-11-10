//! `art query` end to end: the real binary on a real trace written by `TraceWriter`.

use std::path::Path;
use std::process::Command;

use clock::Clock;
use trace_store::{Event, Stamp, TraceWriter};

fn edit(event_id: u64, slot: &str, clock: &[u64], path: &str) -> Event {
    let stamp = Stamp {
        event_id,
        slot: slot.into(),
        session_id: format!("sess-{slot}"),
        vclock: Clock::from_entries(clock.to_vec()).to_bytes(),
        wall_time: 0,
    };
    let input = format!(r#"{{"file_path":"{path}"}}"#);
    Event::before_tool(stamp, "Edit".into(), format!("t{event_id}"), input)
}

fn art(run_dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_art"))
        .arg("query")
        .arg(run_dir)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn query_answers_each_question_about_a_planted_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    for e in [
        edit(1, "slot-1", &[1, 0], "/work/src/lib.rs"),
        edit(2, "slot-2", &[0, 1], "/work/src/lib.rs"),
        edit(3, "slot-2", &[1, 2], "/work/src/lib.rs"),
    ] {
        w.append(&e).unwrap();
    }
    drop(w);
    let ok = |args: &[&str]| {
        let (code, stdout, stderr) = art(tmp.path(), args);
        assert_eq!((code, stderr.as_str()), (0, ""), "{args:?}");
        stdout
    };

    assert_eq!(
        ok(&["concurrent-writes"]),
        "/work/src/lib.rs\t1 (slot-1)\t2 (slot-2)\n1 concurrent write pairs\n"
    );
    assert_eq!(ok(&["happens-before", "1", "3"]), "true\n");
    assert_eq!(ok(&["happens-before", "3", "1"]), "false\n");
    assert_eq!(ok(&["concurrent", "1", "2"]), "true\n");
    assert_eq!(ok(&["knew", "slot-2", "1", "3"]), "true\n");
    assert_eq!(ok(&["knew", "slot-2", "1", "2"]), "false\n");
    assert_eq!(ok(&["causal-past", "3"]), "1\n2\n");
}

#[test]
fn a_bad_question_or_run_exits_2_with_usage() {
    let tmp = tempfile::tempdir().unwrap();
    TraceWriter::create(tmp.path()).unwrap();
    for args in [
        &["no-such-question"][..],
        &["happens-before", "1"],
        &["happens-before", "1", "x"],
        &["happens-before", "1", "2"],
    ] {
        let (code, stdout, stderr) = art(tmp.path(), args);
        assert_eq!((code, stdout.as_str()), (2, ""), "{args:?}");
        assert!(
            stderr.starts_with("art: ") && stderr.contains("art query <run-dir>"),
            "{stderr}"
        );
    }
    let (code, _, stderr) = art(&tmp.path().join("missing"), &["concurrent-writes"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("could not load the run"), "{stderr}");
}
