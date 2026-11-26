use crate::environment::LocalEnvironment;
pub use crate::workspace::LocalWorkspace;
use async_trait::async_trait;
use autra_core::{EffectClass, ToolSpec};
pub use autra_execution::tool::{
    TOOL_PROTOCOL_VERSION, Tool, ToolContext, ToolContract, ToolDescriptor, ToolError, ToolInput,
    ToolOutput, ToolRegistry,
};
use autra_execution::{ExecutionEnvironment, FileKind};
use serde::Deserialize;
use std::sync::Arc;

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
    environment: Arc<dyn ExecutionEnvironment>,
    max_bytes: usize,
}

impl ReadFileTool {
    pub fn in_environment(environment: Arc<dyn ExecutionEnvironment>) -> Self {
        Self {
            environment,
            max_bytes: DEFAULT_READ_BYTES,
        }
    }

    pub fn new(workspace: LocalWorkspace) -> Self {
        Self {
            environment: Arc::new(LocalEnvironment::new(workspace)),
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
        let read = self
            .environment
            .read_file(&args.path, self.max_bytes)
            .await?;
        let bytes = read.bytes;
        let truncated = read.truncated;
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
    environment: Arc<dyn ExecutionEnvironment>,
}

impl ListFilesTool {
    pub fn in_environment(environment: Arc<dyn ExecutionEnvironment>) -> Self {
        Self { environment }
    }

    pub fn new(workspace: LocalWorkspace) -> Self {
        Self::in_environment(Arc::new(LocalEnvironment::new(workspace)))
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
        let mut entries: Vec<String> = self
            .environment
            .list_directory(&args.path)
            .await?
            .into_iter()
            .map(|entry| {
                if entry.kind == FileKind::Directory {
                    format!("{}/", entry.name)
                } else {
                    entry.name
                }
            })
            .collect();
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

const WRITE_FILE_SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string","description":"Workspace-relative file path"},"content":{"type":"string","description":"UTF-8 content to write"}},"required":["path","content"]}"#;
const WRITE_FILE_MAX_BYTES: usize = 1024 * 1024;

/// Writes a UTF-8 file inside the workspace. Requires approval.
pub struct WriteFileTool {
    environment: Arc<dyn ExecutionEnvironment>,
}

impl WriteFileTool {
    pub fn in_environment(environment: Arc<dyn ExecutionEnvironment>) -> Self {
        Self { environment }
    }

    pub fn new(workspace: LocalWorkspace) -> Self {
        Self::in_environment(Arc::new(LocalEnvironment::new(workspace)))
    }
}

#[derive(Deserialize)]
struct WriteFileArgs {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".to_owned(),
            description: "Write UTF-8 content to a workspace file, creating parent directories. Requires approval.".to_owned(),
            input_schema: WRITE_FILE_SCHEMA.to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Idempotent
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn preview(&self, arguments: &str) -> Option<String> {
        let args: WriteFileArgs = serde_json::from_str(arguments).ok()?;
        if args.content.len() > WRITE_FILE_MAX_BYTES {
            return Some(format!(
                "write_file {}: content exceeds {WRITE_FILE_MAX_BYTES} bytes",
                args.path
            ));
        }
        let existing = self
            .environment
            .read_preview(&args.path, WRITE_FILE_MAX_BYTES);
        Some(match existing {
            Some(old) => unified_preview(&args.path, &old, &args.content, PREVIEW_MAX_BYTES),
            None => format!(
                "create {} ({} bytes)
{}",
                args.path,
                args.content.len(),
                first_lines(&args.content, 20)
            ),
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let args: WriteFileArgs = serde_json::from_str(arguments)
            .map_err(|error| invalid_arguments("write_file", error))?;
        if args.content.len() > WRITE_FILE_MAX_BYTES {
            return Err(ToolError::new(format!(
                "content exceeds {WRITE_FILE_MAX_BYTES} bytes"
            )));
        }
        self.environment
            .write_file(&args.path, &args.content)
            .await?;
        Ok(format!(
            "wrote {} bytes to {}",
            args.content.len(),
            args.path
        ))
    }
}

const SEARCH_TEXT_SCHEMA: &str = r#"{"type":"object","properties":{"query":{"type":"string","description":"Text to search for"},"path":{"type":"string","description":"Workspace-relative file or directory (default \".\")"},"case_sensitive":{"type":"boolean","description":"Match case exactly (default false)"},"max_results":{"type":"integer","description":"Maximum matches to return (default 50, max 200)"}},"required":["query"]}"#;
const SEARCH_MAX_RESULTS: usize = 200;
const SEARCH_MAX_FILE_BYTES: u64 = 1024 * 1024;
const SEARCH_MAX_FILES: usize = 20_000;
const SEARCH_MAX_LINE_CHARS: usize = 400;
const SEARCH_SKIP_DIRS: [&str; 4] = [".git", ".autra", "target", "node_modules"];

/// Finds text in workspace files and reports `path:line: text` matches.
pub struct SearchTextTool {
    environment: Arc<dyn ExecutionEnvironment>,
}

impl SearchTextTool {
    pub fn in_environment(environment: Arc<dyn ExecutionEnvironment>) -> Self {
        Self { environment }
    }

    pub fn new(workspace: LocalWorkspace) -> Self {
        Self::in_environment(Arc::new(LocalEnvironment::new(workspace)))
    }
}

#[derive(Clone, Deserialize)]
struct SearchTextArgs {
    query: String,
    #[serde(default = "default_path")]
    path: String,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default = "default_max_results")]
    max_results: usize,
}

fn default_max_results() -> usize {
    50
}

fn truncate_line(line: &str) -> String {
    let mut shortened: String = line.chars().take(SEARCH_MAX_LINE_CHARS).collect();
    if line.chars().count() > SEARCH_MAX_LINE_CHARS {
        shortened.push('…');
    }
    shortened
}

#[async_trait]
impl Tool for SearchTextTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search_text".to_owned(),
            description: "Search UTF-8 files in the workspace and return path:line matches."
                .to_owned(),
            input_schema: SEARCH_TEXT_SCHEMA.to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::ReadOnly
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let args: SearchTextArgs = serde_json::from_str(arguments)
            .map_err(|error| invalid_arguments("search_text", error))?;
        if args.query.is_empty() {
            return Err(ToolError::new("search query must not be empty"));
        }
        search_text_in_environment(self.environment.as_ref(), &args).await
    }
}

async fn search_text_in_environment(
    environment: &dyn ExecutionEnvironment,
    args: &SearchTextArgs,
) -> Result<String, ToolError> {
    environment.metadata(&args.path).await?;
    let needle = if args.case_sensitive {
        args.query.clone()
    } else {
        args.query.to_lowercase()
    };
    let limit = args.max_results.clamp(1, SEARCH_MAX_RESULTS);
    let mut matches = Vec::new();
    let mut files_read = 0;
    let mut truncated = false;
    let mut stack = vec![args.path.clone()];
    'walk: while let Some(path) = stack.pop() {
        let metadata = match environment.metadata(&path).await {
            Ok(m) => m,
            Err(_) => continue,
        };
        if metadata.kind == FileKind::Directory {
            let entries = match environment.list_directory(&path).await {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries {
                if entry.kind == FileKind::Symlink
                    || (entry.kind == FileKind::Directory
                        && SEARCH_SKIP_DIRS.contains(&entry.name.as_str()))
                {
                    continue;
                }
                stack.push(format!("{path}/{}", entry.name));
            }
            continue;
        }
        if metadata.kind != FileKind::File || metadata.len > SEARCH_MAX_FILE_BYTES {
            continue;
        }
        files_read += 1;
        if files_read > SEARCH_MAX_FILES {
            truncated = true;
            break;
        }
        let read = match environment
            .read_file(&path, SEARCH_MAX_FILE_BYTES as usize)
            .await
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        if read.truncated {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&read.bytes) else {
            continue;
        };
        let relative = path.strip_prefix("./").unwrap_or(&path);
        for (index, line) in text.lines().enumerate() {
            let matched = if args.case_sensitive {
                line.contains(&needle)
            } else {
                line.to_lowercase().contains(&needle)
            };
            if matched {
                matches.push(format!("{relative}:{}: {}", index + 1, truncate_line(line)));
                if matches.len() >= limit {
                    truncated = true;
                    break 'walk;
                }
            }
        }
    }
    if matches.is_empty() {
        return Ok(format!("no matches for `{}`", args.query));
    }
    let mut output = matches.join("\n");
    if truncated {
        output.push_str(&format!("\n[stopped at {limit} matches]"));
    }
    Ok(output)
}

const PREVIEW_MAX_BYTES: usize = 4 * 1024;

/// Renders a bounded unified diff for approval previews and tool results.
pub fn unified_preview(path: &str, old: &str, new: &str, max_bytes: usize) -> String {
    let diff = similar::TextDiff::from_lines(old, new);
    let rendered = diff
        .unified_diff()
        .context_radius(3)
        .header(path, path)
        .to_string();
    if rendered.len() <= max_bytes {
        return rendered;
    }
    let mut end = max_bytes;
    while end > 0 && !rendered.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[diff truncated at {max_bytes} bytes]",
        &rendered[..end]
    )
}

fn first_lines(content: &str, limit: usize) -> String {
    let mut lines: Vec<&str> = content.lines().take(limit).collect();
    let truncated = content.lines().count() > limit;
    if truncated {
        lines.push("…");
    }
    lines.join("\n")
}

const EDIT_FILE_SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string","description":"Workspace-relative file path"},"old_string":{"type":"string","description":"Exact text to replace"},"new_string":{"type":"string","description":"Replacement text"},"replace_all":{"type":"boolean","description":"Replace every occurrence (default false; ambiguous edits are rejected)"}},"required":["path","old_string","new_string"]}"#;
const EDIT_FILE_MAX_BYTES: usize = 1024 * 1024;

/// Replaces an exact string in a workspace file. Requires approval.
pub struct EditFileTool {
    environment: Arc<dyn ExecutionEnvironment>,
}

impl EditFileTool {
    pub fn in_environment(environment: Arc<dyn ExecutionEnvironment>) -> Self {
        Self { environment }
    }

    pub fn new(workspace: LocalWorkspace) -> Self {
        Self::in_environment(Arc::new(LocalEnvironment::new(workspace)))
    }

    fn replacement(
        &self,
        args: &EditFileArgs,
        content: String,
    ) -> Result<(String, String, usize), ToolError> {
        if args.old_string.is_empty() {
            return Err(ToolError::new("old_string must not be empty"));
        }
        if args.old_string == args.new_string {
            return Err(ToolError::new(
                "old_string and new_string are identical; there is nothing to change",
            ));
        }
        if content.len() > EDIT_FILE_MAX_BYTES {
            return Err(ToolError::new(format!(
                "`{}` exceeds {EDIT_FILE_MAX_BYTES} bytes",
                args.path
            )));
        }
        let count = content.matches(&args.old_string).count();
        if count == 0 {
            return Err(ToolError::new(format!(
                "old_string not found in `{}`",
                args.path
            )));
        }
        if count > 1 && !args.replace_all {
            return Err(ToolError::new(format!(
                "old_string matches {count} times in `{}`; provide more context or set replace_all",
                args.path
            )));
        }
        let updated = if args.replace_all {
            content.replace(&args.old_string, &args.new_string)
        } else {
            content.replacen(&args.old_string, &args.new_string, 1)
        };
        if updated.len() > EDIT_FILE_MAX_BYTES {
            return Err(ToolError::new(format!(
                "edited `{}` would exceed {EDIT_FILE_MAX_BYTES} bytes",
                args.path
            )));
        }
        Ok((content, updated, count))
    }
}

#[derive(Deserialize)]
struct EditFileArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl Tool for EditFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit_file".to_owned(),
            description: "Replace an exact string in a workspace file. Requires approval."
                .to_owned(),
            input_schema: EDIT_FILE_SCHEMA.to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Idempotent
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn preview(&self, arguments: &str) -> Option<String> {
        let args: EditFileArgs = serde_json::from_str(arguments).ok()?;
        let Some(content) = self
            .environment
            .read_preview(&args.path, EDIT_FILE_MAX_BYTES)
        else {
            return Some(format!(
                "edit {}: replace {:?} with {:?}",
                args.path, args.old_string, args.new_string
            ));
        };
        Some(match self.replacement(&args, content) {
            Ok((old, new, count)) => format!(
                "edit {} ({count} occurrence(s))\n{}",
                args.path,
                unified_preview(&args.path, &old, &new, PREVIEW_MAX_BYTES)
            ),
            Err(error) => format!("edit {} will be rejected: {}", args.path, error.message),
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let args: EditFileArgs = serde_json::from_str(arguments)
            .map_err(|error| invalid_arguments("edit_file", error))?;
        let read = self
            .environment
            .read_file(&args.path, EDIT_FILE_MAX_BYTES)
            .await?;
        if read.truncated {
            return Err(ToolError::new(format!(
                "`{}` exceeds {EDIT_FILE_MAX_BYTES} bytes",
                args.path
            )));
        }
        let content = String::from_utf8(read.bytes).map_err(|error| {
            ToolError::new(format!("cannot read `{}` as UTF-8: {error}", args.path))
        })?;
        let (old, new, count) = self.replacement(&args, content)?;
        self.environment
            .write_file_if_unchanged(&args.path, &old, &new)
            .await?;
        Ok(format!(
            "replaced {count} occurrence(s) in {}\n{}",
            args.path,
            unified_preview(&args.path, &old, &new, PREVIEW_MAX_BYTES)
        ))
    }
}
