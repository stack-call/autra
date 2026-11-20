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
