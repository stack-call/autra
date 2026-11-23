use autra_core::{CancelFlag, EffectClass, RunId};
use autra_harness::{LocalWorkspace, RunCommandTool, Tool, ToolContext, ToolInput};
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

fn context(cancel: CancelFlag) -> ToolContext {
    ToolContext {
        run_id: RunId::new("run-cancel"),
        call_id: "call-cancel".to_owned(),
        parent_run_id: None,
        cancel: Some(cancel),
        budget: None,
        child_execution: None,
    }
}

#[tokio::test]
async fn cancellation_stops_the_command() {
    let dir = tempdir().unwrap();
    let tool = RunCommandTool::new(LocalWorkspace::open(dir.path()).unwrap());
    let cancel = CancelFlag::new();
    #[cfg(windows)]
    let command = "ping -n 10 127.0.0.1 > nul";
    #[cfg(not(windows))]
    let command = "sleep 10";
    let input = ToolInput {
        arguments: format!(r#"{{"command":"{command}"}}"#),
    };
    let context = context(cancel.clone());

    let task = tokio::spawn(async move { tool.execute(input, context).await });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    cancel.cancel();
    let result = task.await.unwrap();

    let error = match result {
        Ok(_) => panic!("cancelled command must fail"),
        Err(error) => error,
    };
    assert!(error.outcome_unknown);
    assert!(error.to_string().contains("cancelled"));
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_kills_grandchildren() {
    let dir = tempdir().unwrap();
    let tool = RunCommandTool::new(LocalWorkspace::open(dir.path()).unwrap());
    let cancel = CancelFlag::new();
    let input = ToolInput {
        arguments: r#"{"command":"sleep 30 & echo $! > grandchild.pid; wait"}"#.to_owned(),
    };
    let context = context(cancel.clone());

    let task = tokio::spawn(async move { tool.execute(input, context).await });
    let pid_path = dir.path().join("grandchild.pid");
    for _ in 0..50 {
        if pid_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let pid = std::fs::read_to_string(&pid_path).unwrap();
    cancel.cancel();
    let _ = task.await.unwrap();

    let alive = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .unwrap()
        .success();
    assert!(!alive, "grandchild {} survived cancellation", pid.trim());
}
