use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Arc;

use async_trait::async_trait;
use autra_core::{EffectClass, ToolSpec};
use serde::Deserialize;
use thiserror::Error;

/// A capability the model may request.
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    fn effect_class(&self) -> EffectClass {
        EffectClass::Unknown
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError>;
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct ToolError {
    pub message: String,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: impl Tool + 'static) -> &mut Self {
        self.tools.insert(tool.spec().name, Arc::new(tool));
        self
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(String::as_str).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Returns its text argument unchanged. Useful for deterministic tests.
pub struct EchoTool;

#[derive(Deserialize)]
struct EchoArgs {
    text: String,
}

const ECHO_SCHEMA: &str = r#"{"type":"object","properties":{"text":{"type":"string","description":"Text to return unchanged"}},"required":["text"]}"#;

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "echo".to_owned(),
            description: "Return the provided text unchanged.".to_owned(),
            input_schema: ECHO_SCHEMA.to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::ReadOnly
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let args: EchoArgs =
            serde_json::from_str(arguments).map_err(|error| invalid_arguments("echo", error))?;
        Ok(args.text)
    }
}

/// A filesystem root that read-only tools are confined to.
///
/// Confinement is user-space path validation, not a sandbox: it does not
/// isolate processes, mounts or the network. Use a trusted Sandbox backend
/// when isolation is required.
#[derive(Clone, Debug)]
pub struct LocalWorkspace {
    root: std::path::PathBuf,
}

impl LocalWorkspace {
    pub fn open(root: impl AsRef<std::path::Path>) -> Result<Self, ToolError> {
        let root = root.as_ref();
        let root = std::fs::canonicalize(root).map_err(|error| {
            ToolError::new(format!(
                "cannot open workspace `{}`: {error}",
                root.display()
            ))
        })?;
        if !root.is_dir() {
            return Err(ToolError::new(format!(
                "workspace root `{}` is not a directory",
                root.display()
            )));
        }
        Ok(Self { root })
    }

    fn resolve(&self, relative: &str) -> Result<std::path::PathBuf, ToolError> {
        use std::path::Component;

        let relative_path = std::path::Path::new(relative);
        if relative_path.is_absolute() {
            return Err(ToolError::new(format!(
                "absolute paths are not allowed: `{relative}`"
            )));
        }
        for component in relative_path.components() {
            if matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            ) {
                return Err(ToolError::new(format!(
                    "path must stay inside the workspace: `{relative}`"
                )));
            }
        }
        let candidate = self.root.join(relative_path);
        let resolved = std::fs::canonicalize(&candidate)
            .map_err(|error| ToolError::new(format!("cannot resolve `{relative}`: {error}")))?;
        if !resolved.starts_with(&self.root) {
            return Err(ToolError::new(format!(
                "path escapes the workspace: `{relative}`"
            )));
        }
        Ok(resolved)
    }
}

fn truncate_at_char_boundary(bytes: &[u8], max_bytes: usize) -> &[u8] {
    if bytes.len() <= max_bytes {
        return bytes;
    }
    let mut end = max_bytes;
    while end > 0 && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
        end -= 1;
    }
    &bytes[..end]
}

fn invalid_arguments(tool: &str, error: serde_json::Error) -> ToolError {
    ToolError::new(format!("invalid {tool} arguments: {error}"))
}

const READ_FILE_SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string","description":"Workspace-relative file path"}},"required":["path"]}"#;
const DEFAULT_READ_BYTES: usize = 32 * 1024;

pub struct ReadFileTool {
    workspace: LocalWorkspace,
    max_bytes: usize,
}

impl ReadFileTool {
    pub fn new(workspace: LocalWorkspace) -> Self {
        Self {
            workspace,
            max_bytes: DEFAULT_READ_BYTES,
        }
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

#[derive(Deserialize)]
struct ReadFileArgs {
    path: String,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".to_owned(),
            description: "Read a UTF-8 text file from the workspace.".to_owned(),
            input_schema: READ_FILE_SCHEMA.to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::ReadOnly
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let args: ReadFileArgs = serde_json::from_str(arguments)
            .map_err(|error| invalid_arguments("read_file", error))?;
        let path = self.workspace.resolve(&args.path)?;
        let metadata = std::fs::metadata(&path)
            .map_err(|error| ToolError::new(format!("cannot read `{}`: {error}", args.path)))?;
        if !metadata.is_file() {
            return Err(ToolError::new(format!("`{}` is not a file", args.path)));
        }
        let file = std::fs::File::open(&path)
            .map_err(|error| ToolError::new(format!("cannot read `{}`: {error}", args.path)))?;
        let mut bytes = Vec::new();
        file.take((self.max_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| ToolError::new(format!("cannot read `{}`: {error}", args.path)))?;
        let truncated = bytes.len() > self.max_bytes;
        let slice = if truncated {
            truncate_at_char_boundary(&bytes, self.max_bytes)
        } else {
            &bytes[..]
        };
        let mut text = String::from_utf8_lossy(slice).into_owned();
        if truncated {
            text.push_str(&format!("\n[truncated at {} bytes]", self.max_bytes));
        }
        Ok(text)
    }
}

const LIST_FILES_SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string","description":"Workspace-relative directory path (default \".\")"}}}"#;
const LIST_FILES_MAX_ENTRIES: usize = 200;

pub struct ListFilesTool {
    workspace: LocalWorkspace,
}

impl ListFilesTool {
    pub fn new(workspace: LocalWorkspace) -> Self {
        Self { workspace }
    }
}

#[derive(Deserialize)]
struct ListFilesArgs {
    #[serde(default = "default_path")]
    path: String,
}

fn default_path() -> String {
    ".".to_owned()
}

#[async_trait]
impl Tool for ListFilesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_files".to_owned(),
            description: "List one directory of the workspace.".to_owned(),
            input_schema: LIST_FILES_SCHEMA.to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::ReadOnly
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let args: ListFilesArgs = if arguments.trim().is_empty() {
            ListFilesArgs {
                path: default_path(),
            }
        } else {
            serde_json::from_str(arguments)
                .map_err(|error| ToolError::new(format!("invalid list_files arguments: {error}")))?
        };
        let path = self.workspace.resolve(&args.path)?;
        let metadata = std::fs::metadata(&path)
            .map_err(|error| ToolError::new(format!("cannot list `{}`: {error}", args.path)))?;
        if !metadata.is_dir() {
            return Err(ToolError::new(format!(
                "`{}` is not a directory",
                args.path
            )));
        }

        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&path)
            .map_err(|error| ToolError::new(format!("cannot list `{}`: {error}", args.path)))?
        {
            let entry = entry
                .map_err(|error| ToolError::new(format!("cannot list `{}`: {error}", args.path)))?;
            let file_type = entry
                .file_type()
                .map_err(|error| ToolError::new(format!("cannot list `{}`: {error}", args.path)))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            entries.push(if file_type.is_dir() {
                format!("{name}/")
            } else {
                name
            });
        }
        entries.sort();

        let truncated = entries.len() > LIST_FILES_MAX_ENTRIES;
        entries.truncate(LIST_FILES_MAX_ENTRIES);
        if entries.is_empty() {
            return Ok("(empty directory)".to_owned());
        }
        let mut output = entries.join("\n");
        if truncated {
            output.push_str(&format!(
                "\n[truncated at {LIST_FILES_MAX_ENTRIES} entries]"
            ));
        }
        Ok(output)
    }
}
