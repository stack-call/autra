use autra_core::EffectClass;
use autra_harness::{LocalWorkspace, Tool};
use autra_runtime::RunCommandTool;
use tempfile::tempdir;

#[tokio::test]
async fn run_command_returns_exit_code_and_output() {
    let dir = tempdir().unwrap();
    let tool = RunCommandTool::new(LocalWorkspace::open(dir.path()).unwrap());

    let output = tool.call(r#"{"command":"echo hello"}"#).await.unwrap();

    assert!(tool.requires_approval());
    assert_eq!(tool.effect_class(), EffectClass::NonIdempotent);
    assert!(output.contains("exit code: 0"));
    assert!(output.contains("hello"));
}

#[tokio::test]
async fn run_command_runs_in_the_workspace_root() {
    let dir = tempdir().unwrap();
    let tool = RunCommandTool::new(LocalWorkspace::open(dir.path()).unwrap());

    tool.call(r#"{"command":"echo created > marker.txt"}"#)
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("marker.txt"))
            .unwrap()
            .trim(),
        "created"
    );
}

#[tokio::test]
async fn a_non_zero_exit_is_an_error_with_its_output() {
    let dir = tempdir().unwrap();
    let tool = RunCommandTool::new(LocalWorkspace::open(dir.path()).unwrap());
    #[cfg(windows)]
    let command = r#"{"command":"echo broken && exit /b 3"}"#;
    #[cfg(not(windows))]
    let command = r#"{"command":"echo broken; exit 3"}"#;

    let error = tool.call(command).await.unwrap_err();

    assert!(!error.outcome_unknown);
    assert!(error.to_string().contains("exit code: 3"));
    assert!(error.to_string().contains("broken"));
}

#[tokio::test]
async fn a_timeout_is_an_unknown_outcome() {
    let dir = tempdir().unwrap();
    let tool = RunCommandTool::new(LocalWorkspace::open(dir.path()).unwrap());
    #[cfg(windows)]
    let command = r#"{"command":"ping -n 5 127.0.0.1 > nul","timeout_secs":1}"#;
    #[cfg(not(windows))]
    let command = r#"{"command":"sleep 5","timeout_secs":1}"#;

    let error = tool.call(command).await.unwrap_err();

    assert!(error.outcome_unknown);
    assert!(error.to_string().contains("timed out"));
}
