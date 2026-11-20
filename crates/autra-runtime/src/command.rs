//! Local workspace command execution.
//!
//! This is a user-space convenience tool, not a sandbox. It runs an
//! unsandboxed shell in the workspace root; use it only with approval.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use autra_core::{EffectClass, ToolSpec};
use autra_harness::{LocalWorkspace, Tool, ToolError};
use serde::Deserialize;

pub const RUN_COMMAND_TOOL: &str = "run_command";

const RUN_COMMAND_SCHEMA: &str = r#"{"type":"object","properties":{"command":{"type":"string","description":"Shell command to run in the workspace root"},"timeout_secs":{"type":"integer","description":"Timeout in seconds (default 60, max 300)"}},"required":["command"]}"#;
const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 300;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;

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
        let mut command = shell_command(&args.command);
        command
            .current_dir(self.workspace.root())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command
            .spawn()
            .map_err(|error| ToolError::new(format!("cannot start command: {error}")))?;
        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                return Err(ToolError::new(format!("command failed to run: {error}")));
            }
            Err(_) => {
                return Err(ToolError::outcome_unknown(format!(
                    "command timed out after {timeout_secs}s; it may still have changed the workspace"
                )));
            }
        };
        let mut report = format!(
            "exit code: {}",
            output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "terminated by signal".to_owned())
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stdout.trim().is_empty() {
            report.push_str("\nstdout:\n");
            report.push_str(stdout.trim_end());
        }
        if !stderr.trim().is_empty() {
            report.push_str("\nstderr:\n");
            report.push_str(stderr.trim_end());
        }
        let report = truncate_output(&report, self.max_output_bytes);
        if output.status.success() {
            Ok(report)
        } else {
            Err(ToolError::new(report))
        }
    }
}

fn shell_command(command: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut shell = tokio::process::Command::new("cmd");
        shell.arg("/C").arg(command);
        shell
    }
    #[cfg(not(windows))]
    {
        let mut shell = tokio::process::Command::new("sh");
        shell.arg("-c").arg(command);
        shell
    }
}

fn truncate_output(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[output truncated at {max_bytes} bytes]", &text[..end])
}
