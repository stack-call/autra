//! Read-only git tools for workspace inspection.

use crate::environment::LocalEnvironment;
use std::sync::Arc;
use std::time::Duration;

use crate::{LocalWorkspace, Tool, ToolError};
use async_trait::async_trait;
use autra_core::{EffectClass, ToolSpec};
use autra_execution::{
    CommandOutput as ProcessOutput, EnvironmentCommand, ExecutionEnvironment, ToolContext,
    ToolInput, ToolOutput, ToolResource,
};
use serde::Deserialize;

use crate::command::truncate_output;

const GIT_TIMEOUT: Duration = Duration::from_secs(60);
const GIT_MAX_BYTES: usize = 32 * 1024;

const GIT_STATUS_SCHEMA: &str = r#"{"type":"object","properties":{}}"#;
const GIT_DIFF_SCHEMA: &str = r#"{"type":"object","properties":{"staged":{"type":"boolean","description":"Show staged changes instead of unstaged ones"},"path":{"type":"string","description":"Workspace-relative path to limit the diff"}}}"#;

fn ensure_success(label: &str, output: &ProcessOutput) -> Result<(), ToolError> {
    if output.code == Some(0) {
        return Ok(());
    }
    let detail = if output.stderr.trim().is_empty() {
        output.stdout.trim()
    } else {
        output.stderr.trim()
    };
    Err(ToolError::new(format!("{label} failed: {detail}")))
}

/// Shows the workspace git status in short format. Read-only.
pub struct GitStatusTool {
    environment: Arc<dyn ExecutionEnvironment>,
}

impl GitStatusTool {
    pub fn in_environment(environment: Arc<dyn ExecutionEnvironment>) -> Self {
        Self { environment }
    }

    pub fn new(workspace: LocalWorkspace) -> Self {
        Self::in_environment(Arc::new(LocalEnvironment::new(workspace)))
    }
}

#[async_trait]
impl Tool for GitStatusTool {
    fn environment(&self) -> Option<Arc<dyn ExecutionEnvironment>> {
        Some(self.environment.clone())
    }
    fn execution_requirements(&self) -> autra_core::ExecutionRequirements {
        autra_core::ExecutionRequirements {
            environment: Some(autra_core::EnvironmentAffinity::Required(
                self.environment.id().to_owned(),
            )),
            file_api: false,
            command_execution: true,
        }
    }
    async fn execute(
        &self,
        input: ToolInput,
        context: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let tool = Self::in_environment(
            context
                .environment
                .unwrap_or_else(|| self.environment.clone()),
        );
        tool.call(&input.arguments).await.map(ToolOutput::text)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_status".to_owned(),
            description: "Show the workspace git status in short format.".to_owned(),
            input_schema: GIT_STATUS_SCHEMA.to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::ReadOnly
    }

    fn resource(&self) -> ToolResource {
        ToolResource::Process
    }

    async fn call(&self, _arguments: &str) -> Result<String, ToolError> {
        let mut output = self
            .environment
            .execute(
                EnvironmentCommand::Program {
                    program: "git".to_owned(),
                    args: vec![
                        "status".to_owned(),
                        "--short".to_owned(),
                        "--branch".to_owned(),
                    ],
                },
                GIT_TIMEOUT,
                None,
            )
            .await?;
        // Git for Windows occasionally returns exit 0 with empty output on the
        // first status after `git init`. A read-only retry is safe and avoids
        // reporting a populated tree as clean.
        if output.code == Some(0) && output.stdout.trim().is_empty() {
            output = self
                .environment
                .execute(
                    EnvironmentCommand::Program {
                        program: "git".to_owned(),
                        args: vec![
                            "status".to_owned(),
                            "--short".to_owned(),
                            "--branch".to_owned(),
                        ],
                    },
                    GIT_TIMEOUT,
                    None,
                )
                .await?;
        }
        ensure_success("git status", &output)?;
        let text = output.stdout.trim_end();
        if text.is_empty() {
            Ok("working tree clean".to_owned())
        } else {
            Ok(truncate_output(text, GIT_MAX_BYTES))
        }
    }
}

/// Shows the workspace git diff. Read-only.
pub struct GitDiffTool {
    environment: Arc<dyn ExecutionEnvironment>,
}

impl GitDiffTool {
    pub fn in_environment(environment: Arc<dyn ExecutionEnvironment>) -> Self {
        Self { environment }
    }

    pub fn new(workspace: LocalWorkspace) -> Self {
        Self::in_environment(Arc::new(LocalEnvironment::new(workspace)))
    }
}

#[derive(Deserialize)]
struct GitDiffArgs {
    #[serde(default)]
    staged: bool,
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for GitDiffTool {
    fn environment(&self) -> Option<Arc<dyn ExecutionEnvironment>> {
        Some(self.environment.clone())
    }
    fn execution_requirements(&self) -> autra_core::ExecutionRequirements {
        autra_core::ExecutionRequirements {
            environment: Some(autra_core::EnvironmentAffinity::Required(
                self.environment.id().to_owned(),
            )),
            file_api: true,
            command_execution: true,
        }
    }
    async fn execute(
        &self,
        input: ToolInput,
        context: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let tool = Self::in_environment(
            context
                .environment
                .unwrap_or_else(|| self.environment.clone()),
        );
        tool.call(&input.arguments).await.map(ToolOutput::text)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_diff".to_owned(),
            description: "Show the workspace git diff, optionally staged or for one path."
                .to_owned(),
            input_schema: GIT_DIFF_SCHEMA.to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::ReadOnly
    }

    fn resource(&self) -> ToolResource {
        ToolResource::Process
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let args: GitDiffArgs = if arguments.trim().is_empty() {
            GitDiffArgs {
                staged: false,
                path: None,
            }
        } else {
            serde_json::from_str(arguments)
                .map_err(|error| ToolError::new(format!("invalid git_diff arguments: {error}")))?
        };
        let mut command_args = vec!["diff".to_owned(), "--no-color".to_owned()];
        if args.staged {
            command_args.push("--staged".to_owned());
        }
        if let Some(path) = &args.path {
            self.environment.metadata(path).await?;
            command_args.push("--".to_owned());
            command_args.push(path.clone());
        }
        let output = self
            .environment
            .execute(
                EnvironmentCommand::Program {
                    program: "git".to_owned(),
                    args: command_args,
                },
                GIT_TIMEOUT,
                None,
            )
            .await?;
        ensure_success("git diff", &output)?;
        let text = output.stdout.trim_end();
        if text.is_empty() {
            return Ok(if args.staged {
                "(no staged changes)".to_owned()
            } else {
                "(no unstaged changes)".to_owned()
            });
        }
        Ok(truncate_output(text, GIT_MAX_BYTES))
    }
}
