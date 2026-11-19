use std::path::Path;
use std::process::{Command, Output};

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
