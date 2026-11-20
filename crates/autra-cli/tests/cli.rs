use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn autractl() -> Command {
    Command::new(env!("CARGO_BIN_EXE_autractl"))
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn help_lists_commands() {
    let output = autractl().arg("--help").output().unwrap();

    assert!(output.status.success());
    let output_text = stdout(&output);
    assert!(output_text.contains("autractl run"));
    assert!(output_text.contains("autractl chat"));
    assert!(output_text.contains("autractl demo"));
    assert!(output_text.contains("autractl tools"));
}

#[test]
fn unknown_command_fails_with_usage() {
    let output = autractl().arg("nope").output().unwrap();

    assert!(!output.status.success());
    let error_text = stderr(&output);
    assert!(error_text.contains("unknown command"));
    assert!(error_text.contains("USAGE"));
}

#[test]
fn tools_lists_builtin_tools() {
    let output = autractl().arg("tools").output().unwrap();

    assert!(output.status.success());
    let output_text = stdout(&output);
    assert!(output_text.contains("echo"));
    assert!(output_text.contains("list_files"));
    assert!(output_text.contains("read_file"));
    assert!(output_text.contains("write_file"));
}

#[test]
fn demo_lists_and_reads_the_workspace() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = autractl()
        .arg("demo")
        .current_dir(workspace_root)
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let output_text = stdout(&output);
    assert!(output_text.contains("Rust-native agent framework"));
    assert!(output_text.contains("demo: listed the workspace and read README.md"));
}

#[test]
fn demo_fails_without_a_readme() {
    let dir = tempfile::tempdir().unwrap();
    let output = autractl()
        .arg("demo")
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("demo failed"));
}

#[test]
fn run_with_model_without_api_key_fails() {
    let output = autractl()
        .args(["run", "--model", "test-model", "hello"])
        .env_remove("OPENAI_API_KEY")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("OPENAI_API_KEY"));
}

#[test]
fn run_with_empty_model_name_fails() {
    let output = autractl()
        .args(["run", "--model", "", "hello"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("usage"));
}

#[test]
fn chat_runs_one_turn_per_line() {
    let mut child = autractl()
        .arg("chat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"hello\nagain\n").unwrap();
    }

    let output = child.wait_with_output().unwrap();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert_eq!(text.matches("[model] echo: hello").count(), 1);
    assert_eq!(text.matches("[model] echo: again").count(), 1);
}

#[test]
fn run_uses_the_selected_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let output = autractl()
        .args([
            "run",
            "--workspace",
            dir.path().to_str().unwrap(),
            "--read-only",
            "hello",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("echo: hello"));
    assert!(stderr(&output).contains("[workspace]"));
    assert!(stderr(&output).contains("read-only"));
}

#[test]
fn tools_lists_the_workspace_capabilities() {
    let output = autractl().arg("tools").output().unwrap();

    assert!(output.status.success());
    let output_text = stdout(&output);
    for name in [
        "edit_file",
        "git_diff",
        "git_status",
        "list_files",
        "read_file",
        "search_text",
        "write_file",
        "run_command",
    ] {
        assert!(
            output_text.contains(name),
            "missing {name} in:\n{output_text}"
        );
    }
}

#[test]
fn unknown_options_fail_with_usage() {
    let output = autractl()
        .args(["run", "--readonly", "hello"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let error_text = stderr(&output);
    assert!(error_text.contains("unknown option `--readonly`"));
    assert!(error_text.contains("usage: autractl run"));
}

#[test]
fn run_can_persist_to_a_journal() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("run.db");
    let output = autractl()
        .args([
            "run",
            "--journal",
            journal.to_str().unwrap(),
            "--read-only",
            "hello",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(journal.exists());
}

#[test]
fn resume_requires_a_journal_and_a_model() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("run.db");

    let missing_journal = autractl().args(["resume", "run-1"]).output().unwrap();
    assert!(!missing_journal.status.success());
    assert!(stderr(&missing_journal).contains("--journal"));

    let missing_model = autractl()
        .args(["resume", "--journal", journal.to_str().unwrap(), "run-1"])
        .output()
        .unwrap();
    assert!(!missing_model.status.success());
    assert!(stderr(&missing_model).contains("--model"));
}

#[test]
fn outbox_requires_a_journal() {
    let output = autractl().arg("outbox").output().unwrap();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("--journal"));
}

#[test]
fn outbox_requires_a_model() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("outbox.db");
    let output = autractl()
        .args(["outbox", "--journal", journal.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("--model"));
}

#[test]
fn chat_session_requires_a_journal() {
    let output = autractl()
        .args(["chat", "--session", "session-1"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("--session requires --journal"));
}

#[test]
fn chat_persists_turns_across_processes() {
    use autra::SessionStore;
    use autra_core::SessionId;
    use autra_durable_sqlite::SqliteSessionStore;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("chat.db");
    let run = |input: &[u8]| {
        let mut child = autractl()
            .args([
                "chat",
                "--journal",
                db.to_str().unwrap(),
                "--session",
                "session-1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "stderr: {}", stderr(&output));
        assert!(stderr(&output).contains("[session] session-1"));
    };

    run(b"one\n");
    run(b"two\n");

    let store = SqliteSessionStore::open(&db).unwrap();
    let record = store
        .load_session(&SessionId::new("session-1"))
        .unwrap()
        .unwrap();
    assert_eq!(record.turns.len(), 2);
    assert_eq!(record.turns[0].input, "one");
    assert_eq!(record.turns[1].input, "two");
    assert_eq!(record.turns[1].answer.as_deref(), Some("echo: two"));
}

#[test]
fn a_missing_policy_file_fails() {
    let output = autractl()
        .args(["run", "--policy", "does-not-exist.json", "hello"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("cannot read policy"));
}

#[test]
fn a_valid_policy_file_is_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let policy = dir.path().join("policy.json");
    std::fs::write(
        &policy,
        r#"{"deny_tools":["write_file"],"deny_commands":["git push --force"]}"#,
    )
    .unwrap();
    let output = autractl()
        .args([
            "run",
            "--read-only",
            "--policy",
            policy.to_str().unwrap(),
            "hello",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("echo: hello"));
}

#[test]
fn snapshot_reads_a_journaled_run() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("snapshot.db");
    let run = autractl()
        .args([
            "run",
            "--journal",
            db.to_str().unwrap(),
            "--read-only",
            "hello",
        ])
        .output()
        .unwrap();
    assert!(run.status.success(), "stderr: {}", stderr(&run));
    let run_id = stdout(&run)
        .lines()
        .find_map(|line| line.strip_prefix("[run] "))
        .and_then(|line| line.strip_suffix(" started"))
        .expect("run id in events")
        .to_owned();

    let snapshot = autractl()
        .args(["snapshot", "--journal", db.to_str().unwrap(), &run_id])
        .output()
        .unwrap();

    assert!(snapshot.status.success(), "stderr: {}", stderr(&snapshot));
    let value: serde_json::Value = serde_json::from_slice(&snapshot.stdout).unwrap();
    assert_eq!(value["run_id"], run_id);
    assert_eq!(value["phase"], "Completed");
    assert_eq!(value["final_answer"], "echo: hello");
}
