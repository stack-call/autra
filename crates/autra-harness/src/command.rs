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
    let child = command
        .spawn()
        .map_err(|error| ToolError::new(format!("cannot start `{program}`: {error}")))?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(ProcessOutput {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
        Ok(Err(error)) => Err(ToolError::new(format!(
            "`{program}` failed to run: {error}"
        ))),
        Err(_) => Err(ToolError::outcome_unknown(format!(
            "`{program}` timed out after {}s; it may still have changed the workspace",
            timeout.as_secs()
        ))),
    }
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
