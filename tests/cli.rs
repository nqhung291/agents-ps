//! End-to-end checks against the real binary: exit codes, JSON shape, no hint
//! on a pipe.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agents-ps"))
        .args(args)
        .output()
        .expect("spawn agents-ps")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("utf-8 stdout")
}

#[test]
fn version_comes_from_the_manifest() {
    let out = run(&["--version"]);
    assert!(out.status.success());
    assert_eq!(
        stdout_of(&out).trim(),
        format!("agents-ps {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_lists_every_command() {
    let out = run(&["--help"]);
    assert!(out.status.success());
    let text = stdout_of(&out);
    for needle in [
        "orphans",
        "ports",
        "top",
        "--json",
        "--no-hint",
        "never kills",
    ] {
        assert!(text.contains(needle), "help is missing {needle:?}");
    }
}

#[test]
fn top_without_a_terminal_exits_2() {
    // The harness pipes stdout, so `top` must refuse rather than paint escape codes.
    let out = run(&["top"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains("top needs an interactive terminal"),
        "unexpected stderr: {stderr:?}"
    );
}

#[test]
fn unknown_command_and_flag_exit_2() {
    assert_eq!(run(&["nonsense"]).status.code(), Some(2));
    assert_eq!(run(&["--nonsense"]).status.code(), Some(2));
}

#[test]
fn sessions_json_has_the_stable_shape() {
    let out = run(&["--json"]);
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("valid json");
    assert_eq!(value["schema"], 1);
    assert!(value["generated_at_unix_ms"].is_u64());
    assert!(value["sample_ms"].is_u64());
    assert!(value["sessions"].is_array());
    assert!(value["totals"]["processes"].is_u64());
    for session in value["sessions"].as_array().expect("array") {
        assert!(session["agent_name"].is_string());
        assert!(session["pid"].is_i64());
        assert!(session["processes"].is_array());
        let processes = session["processes"].as_array().expect("array");
        let mut previous: Option<u64> = None;
        for process in processes {
            let depth = process["depth"].as_u64().expect("depth");
            match previous {
                None => assert_eq!(depth, 0, "preorder starts at the root"),
                Some(prev) => assert!(depth <= prev + 1, "preorder depths step by at most one"),
            }
            previous = Some(depth);
            assert!(process["ports"].is_array());
        }
    }
}

#[test]
fn orphans_json_carries_a_manual_cleanup_list() {
    let out = run(&["orphans", "--json"]);
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("valid json");
    assert_eq!(value["schema"], 1);
    assert!(value["orphans"].is_array());
    assert!(value["cleanup"]["terminate"].is_array());
    assert_eq!(
        value["cleanup"]["terminate"].as_array().unwrap().len(),
        value["orphans"].as_array().unwrap().len()
    );
}

#[test]
fn ports_json_is_consistent() {
    let out = run(&["ports", "--json"]);
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("valid json");
    for entry in value["ports"].as_array().expect("array") {
        assert!(entry["port"].as_u64().expect("port") > 0);
        assert!(matches!(entry["kind"].as_str(), Some("session" | "orphan")));
    }
}

#[test]
fn a_pipe_gets_no_ansi_and_no_hint() {
    let out = run(&[]);
    assert!(out.status.success());
    let text = stdout_of(&out);
    assert!(!text.contains('\u{1b}'), "colour leaked into a pipe");
    assert!(
        !text.contains("agentmonitor.haziapp.com"),
        "hint leaked into a pipe"
    );
}
