use std::path::Path;
use std::process::Command;

fn autractl() -> Command {
    Command::new(env!("CARGO_BIN_EXE_autractl"))
}

#[test]
fn help_lists_commands() {
    let output = autractl().arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("autractl run"));
    assert!(stdout.contains("autractl demo"));
    assert!(stdout.contains("autractl tools"));
}

#[test]
fn unknown_command_fails_with_usage() {
    let output = autractl().arg("nope").output().unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown command"));
}

#[test]
fn tools_lists_builtin_tools() {
    let output = autractl().arg("tools").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("echo"));
    assert!(stdout.contains("list_files"));
    assert!(stdout.contains("read_file"));
}

#[test]
fn run_echoes_the_prompt() {
    let output = autractl().arg("run").arg("hello").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("echo: hello"));
    assert!(stdout.contains("status: Completed"));
}

#[test]
fn demo_lists_and_reads_the_workspace() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = autractl().arg("demo").arg(workspace_root).output().unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("list_files"));
    assert!(stdout.contains("read_file"));
    assert!(stdout.contains("status: Completed"));
}
