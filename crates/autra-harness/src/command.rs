//! Local workspace command execution.
//!
//! This is a user-space convenience tool, not a sandbox. It runs an
//! unsandboxed shell in the workspace root; use it only with approval.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crate::{LocalWorkspace, Tool, ToolError};
use async_trait::async_trait;
use autra_core::{EffectClass, ToolSpec};
use serde::Deserialize;

pub const RUN_COMMAND_TOOL: &str = "run_command";

const RUN_COMMAND_SCHEMA: &str = r#"{"type":"object","properties":{"command":{"type":"string","description":"Shell command to run in the workspace root"},"timeout_secs":{"type":"integer","description":"Timeout in seconds (default 60, max 300)"}},"required":["command"]}"#;
const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 300;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;
/// Hard cap per stream while draining. The caller still truncates to its own
/// smaller limit; this only prevents unbounded memory growth.
const CAPTURE_MAX_BYTES: usize = 256 * 1024;

pub(crate) struct ProcessOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs a program in the workspace root with a timeout and captured output.
pub(crate) async fn run_program(
    root: &Path,
    program: &str,
    args: &[String],
    timeout: Duration,
) -> Result<ProcessOutput, ToolError> {
    let mut command = tokio::process::Command::new(program);
    command
        .current_dir(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    sanitize_environment(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| ToolError::new(format!("cannot start `{program}`: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ToolError::new("cannot capture stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ToolError::new("cannot capture stderr"))?;
    let stdout_task = tokio::spawn(read_capped(stdout, CAPTURE_MAX_BYTES));
    let stderr_task = tokio::spawn(read_capped(stderr, CAPTURE_MAX_BYTES));

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            let stdout = stdout_task.await.unwrap_or_default();
            let stderr = stderr_task.await.unwrap_or_default();
            Ok(ProcessOutput {
                code: status.code(),
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            })
        }
        Ok(Err(error)) => {
            kill_tree(&mut child).await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            Err(ToolError::new(format!(
                "`{program}` failed to run: {error}"
            )))
        }
        Err(_) => {
            kill_tree(&mut child).await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            Err(ToolError::outcome_unknown(format!(
                "`{program}` timed out after {}s; it may still have changed the workspace",
                timeout.as_secs()
            )))
        }
    }
}

/// Reads at most `cap` bytes, draining the pipe so the child never blocks.
async fn read_capped<R>(mut reader: R, cap: usize) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut collected = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                if collected.len() < cap {
                    let remaining = cap - collected.len();
                    collected.extend_from_slice(&buffer[..read.min(remaining)]);
                }
            }
            Err(_) => break,
        }
    }
    collected
}

/// Commands must not inherit credentials from the agent process.
fn sanitize_environment(command: &mut tokio::process::Command) {
    let inherited: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os()
        .filter(|(key, _)| !is_sensitive_key(key))
        .collect();
    command.env_clear();
    command.envs(inherited);
}

fn is_sensitive_key(key: &std::ffi::OsStr) -> bool {
    let key = key.to_string_lossy().to_ascii_uppercase();
    key.contains("KEY")
        || key.contains("TOKEN")
        || key.contains("SECRET")
        || key.contains("PASSWORD")
        || key.contains("CREDENTIAL")
        || key.ends_with("_AUTH")
}

async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    {
        if let Some(pid) = child.id() {
            let _ = tokio::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }
    let _ = child.kill().await;
}

pub(crate) fn truncate_output(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[output truncated at {max_bytes} bytes]", &text[..end])
}

pub(crate) fn format_output(output: &ProcessOutput, max_bytes: usize) -> String {
    let mut report = format!(
        "exit code: {}",
        output
            .code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "terminated by signal".to_owned())
    );
    if !output.stdout.trim().is_empty() {
        report.push_str("\nstdout:\n");
        report.push_str(output.stdout.trim_end());
    }
    if !output.stderr.trim().is_empty() {
        report.push_str("\nstderr:\n");
        report.push_str(output.stderr.trim_end());
    }
    truncate_output(&report, max_bytes)
}

/// Runs a shell command in the workspace root and returns its output.
/// It requires approval and is classified `NonIdempotent`.
pub struct RunCommandTool {
    workspace: LocalWorkspace,
    default_timeout: Duration,
    max_output_bytes: usize,
}

impl RunCommandTool {
    pub fn new(workspace: LocalWorkspace) -> Self {
        Self {
            workspace,
            default_timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            max_output_bytes: MAX_OUTPUT_BYTES,
        }
    }

    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }
}

#[derive(Deserialize)]
struct RunCommandArgs {
    command: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[async_trait]
impl Tool for RunCommandTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: RUN_COMMAND_TOOL.to_owned(),
            description: "Run a shell command in the workspace root and return its exit code and output. Requires approval."
                .to_owned(),
            input_schema: RUN_COMMAND_SCHEMA.to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::NonIdempotent
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn preview(&self, arguments: &str) -> Option<String> {
        let args: RunCommandArgs = serde_json::from_str(arguments).ok()?;
        Some(format!("run: {}", args.command))
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let args: RunCommandArgs = serde_json::from_str(arguments)
            .map_err(|error| ToolError::new(format!("invalid run_command arguments: {error}")))?;
        if args.command.trim().is_empty() {
            return Err(ToolError::new("command must not be empty"));
        }
        let timeout_secs = args
            .timeout_secs
            .unwrap_or(self.default_timeout.as_secs())
            .clamp(1, MAX_TIMEOUT_SECS);
        let timeout = Duration::from_secs(timeout_secs);
        let (program, flag) = if cfg!(windows) {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };
        let output = run_program(
            self.workspace.root(),
            program,
            &[flag.to_owned(), args.command.clone()],
            timeout,
        )
        .await?;
        let report = format_output(&output, self.max_output_bytes);
        if output.code == Some(0) {
            Ok(report)
        } else {
            Err(ToolError::new(report))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_sensitive_key, read_capped};
    use std::ffi::OsStr;

    #[test]
    fn credential_environment_variables_are_filtered() {
        assert!(is_sensitive_key(OsStr::new("OPENAI_API_KEY")));
        assert!(is_sensitive_key(OsStr::new("GITHUB_TOKEN")));
        assert!(is_sensitive_key(OsStr::new("AWS_SECRET_ACCESS_KEY")));
        assert!(is_sensitive_key(OsStr::new("SERVICE_PASSWORD")));
        assert!(!is_sensitive_key(OsStr::new("PATH")));
        assert!(!is_sensitive_key(OsStr::new("GIT_AUTHOR_NAME")));
        assert!(!is_sensitive_key(OsStr::new("RUSTUP_HOME")));
    }

    #[tokio::test]
    async fn captured_output_is_bounded_while_the_pipe_is_drained() {
        let input = std::io::Cursor::new(vec![b'x'; 4096]);

        let collected = read_capped(input, 32).await;

        assert_eq!(collected.len(), 32);
    }
}
