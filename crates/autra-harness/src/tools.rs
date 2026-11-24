use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use autra_core::{EffectClass, ToolSpec};
use serde::Deserialize;

pub use autra_execution::tool::{
    TOOL_PROTOCOL_VERSION, Tool, ToolContext, ToolContract, ToolDescriptor, ToolError, ToolInput,
    ToolOutput, ToolRegistry,
};

/// Serializes workspace writes inside one process. Cross-process writers still
/// need the durable journal lease; this only removes in-process races.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[cfg(feature = "process")]
async fn run_blocking<F, T>(task: F) -> Result<T, ToolError>
where
    F: FnOnce() -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|error| ToolError::new(format!("blocking task failed: {error}")))?
}

#[cfg(not(feature = "process"))]
async fn run_blocking<F, T>(task: F) -> Result<T, ToolError>
where
    F: FnOnce() -> Result<T, ToolError>,
{
    task()
}

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

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve(&self, relative: &str) -> Result<std::path::PathBuf, ToolError> {
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

    fn resolve_for_write(&self, relative: &str) -> Result<std::path::PathBuf, ToolError> {
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
        if let Ok(metadata) = std::fs::symlink_metadata(&candidate) {
            if metadata.file_type().is_symlink() {
                return Err(ToolError::new(format!(
                    "refusing to write through symlink `{relative}`"
                )));
            }
        }
        if let Ok(existing) = std::fs::canonicalize(&candidate) {
            if !existing.starts_with(&self.root) {
                return Err(ToolError::new(format!(
                    "path escapes the workspace: `{relative}`"
                )));
            }
            return Ok(existing);
        }

        let parent = candidate
            .parent()
            .ok_or_else(|| ToolError::new(format!("path has no parent directory: `{relative}`")))?;
        let file_name = candidate
            .file_name()
            .ok_or_else(|| ToolError::new(format!("path has no file name: `{relative}`")))?;
        let parent = self.create_parents(parent, relative)?;
        Ok(parent.join(file_name))
    }

    fn create_parents(
        &self,
        parent: &Path,
        relative: &str,
    ) -> Result<std::path::PathBuf, ToolError> {
        if parent.exists() {
            let canonical = std::fs::canonicalize(parent).map_err(|error| {
                ToolError::new(format!("cannot resolve parent of `{relative}`: {error}"))
            })?;
            if !canonical.starts_with(&self.root) {
                return Err(ToolError::new(format!(
                    "path escapes the workspace: `{relative}`"
                )));
            }
            return Ok(canonical);
        }
        let mut existing = parent;
        while !existing.exists() {
            existing = existing
                .parent()
                .ok_or_else(|| ToolError::new(format!("cannot resolve parent of `{relative}`")))?;
        }
        let canonical = std::fs::canonicalize(existing).map_err(|error| {
            ToolError::new(format!("cannot resolve parent of `{relative}`: {error}"))
        })?;
        if !canonical.starts_with(&self.root) {
            return Err(ToolError::new(format!(
                "path escapes the workspace: `{relative}`"
            )));
        }
        let remainder = parent
            .strip_prefix(existing)
            .unwrap_or_else(|_| Path::new(""));
        let target = canonical.join(remainder);
        std::fs::create_dir_all(&target).map_err(|error| {
            ToolError::new(format!("cannot create parent of `{relative}`: {error}"))
        })?;
        let target = std::fs::canonicalize(&target).map_err(|error| {
            ToolError::new(format!("cannot resolve parent of `{relative}`: {error}"))
        })?;
        if !target.starts_with(&self.root) {
            return Err(ToolError::new(format!(
                "path escapes the workspace: `{relative}`"
            )));
        }
        Ok(target)
    }

    /// Write through a temporary file in the same directory and rename it into
    /// place, so a crash leaves either the old or the new contents. The process
    /// lock is backed by an OS file lock so separate local processes serialize
    /// writes to the same workspace.
    pub fn write_atomic(&self, relative: &str, content: &str) -> Result<(), ToolError> {
        let _guard = WRITE_LOCK
            .lock()
            .map_err(|_| ToolError::new("workspace write lock poisoned"))?;
        let _file_lock = self.lock_workspace()?;
        let path = self.resolve_for_write(relative)?;
        self.write_atomic_locked(&path, relative, content.as_bytes())
    }

    /// Atomic write that refuses to overwrite content changed after the caller
    /// read it. This closes the read-modify-write race for `edit_file`.
    pub fn write_atomic_if_unchanged(
        &self,
        relative: &str,
        expected: &str,
        content: &str,
    ) -> Result<(), ToolError> {
        let _guard = WRITE_LOCK
            .lock()
            .map_err(|_| ToolError::new("workspace write lock poisoned"))?;
        let _file_lock = self.lock_workspace()?;
        let path = self.resolve_for_write(relative)?;
        let current = std::fs::read_to_string(&path).map_err(|error| {
            ToolError::new(format!("cannot read `{relative}` before writing: {error}"))
        })?;
        if current != expected {
            return Err(ToolError::new(format!(
                "`{relative}` changed since it was read; retry the edit"
            )));
        }
        self.write_atomic_locked(&path, relative, content.as_bytes())
    }

    fn lock_workspace(&self) -> Result<File, ToolError> {
        use fs2::FileExt;

        let directory = self.root.join(".autra");
        std::fs::create_dir_all(&directory).map_err(|error| {
            ToolError::new(format!(
                "cannot create workspace lock directory `{}`: {error}",
                directory.display()
            ))
        })?;
        let path = directory.join("workspace.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| {
                ToolError::new(format!(
                    "cannot open workspace lock `{}`: {error}",
                    path.display()
                ))
            })?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(file),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(ToolError::new(format!(
                            "workspace `{}` is locked by another writer",
                            self.root.display()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => {
                    return Err(ToolError::new(format!(
                        "cannot lock workspace `{}`: {error}",
                        self.root.display()
                    )));
                }
            }
        }
    }

    fn write_atomic_locked(
        &self,
        path: &Path,
        relative: &str,
        content: &[u8],
    ) -> Result<(), ToolError> {
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            if metadata.file_type().is_symlink() {
                return Err(ToolError::new(format!(
                    "refusing to write through symlink `{relative}`"
                )));
            }
            if !metadata.is_file() {
                return Err(ToolError::new(format!("`{relative}` is not a file")));
            }
        }
        let parent = path
            .parent()
            .ok_or_else(|| ToolError::new(format!("path has no parent: `{relative}`")))?;
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        let temp = parent.join(format!(
            ".autra-write-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let write = || -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            if let Ok(metadata) = std::fs::metadata(path) {
                let _ = file.set_permissions(metadata.permissions());
            }
            file.write_all(content)?;
            file.sync_all()?;
            Ok(())
        };
        if let Err(error) = write() {
            let _ = std::fs::remove_file(&temp);
            return Err(ToolError::new(format!(
                "cannot write `{relative}`: {error}"
            )));
        }
        if let Err(error) = std::fs::rename(&temp, path) {
            let _ = std::fs::remove_file(&temp);
            return Err(ToolError::new(format!(
                "cannot replace `{relative}`: {error}"
            )));
        }
        #[cfg(unix)]
        {
            if let Ok(directory) = std::fs::File::open(parent) {
                let _ = directory.sync_all();
            }
        }
        Ok(())
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

const WRITE_FILE_SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string","description":"Workspace-relative file path"},"content":{"type":"string","description":"UTF-8 content to write"}},"required":["path","content"]}"#;
const WRITE_FILE_MAX_BYTES: usize = 1024 * 1024;

/// Writes a UTF-8 file inside the workspace. Requires approval.
pub struct WriteFileTool {
    workspace: LocalWorkspace,
}

impl WriteFileTool {
    pub fn new(workspace: LocalWorkspace) -> Self {
        Self { workspace }
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
            .workspace
            .resolve(&args.path)
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok());
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
        let workspace = self.workspace.clone();
        let path = args.path.clone();
        let content = args.content.clone();
        run_blocking(move || workspace.write_atomic(&path, &content)).await?;
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
    workspace: LocalWorkspace,
}

impl SearchTextTool {
    pub fn new(workspace: LocalWorkspace) -> Self {
        Self { workspace }
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
        let start = self.workspace.resolve(&args.path)?;
        let workspace = self.workspace.clone();
        let search = args.clone();
        return run_blocking(move || search_text_in_workspace(&workspace, &search, start)).await;
    }
}

fn search_text_in_workspace(
    workspace: &LocalWorkspace,
    args: &SearchTextArgs,
    start: PathBuf,
) -> Result<String, ToolError> {
    let needle = if args.case_sensitive {
        args.query.clone()
    } else {
        args.query.to_lowercase()
    };
    let limit = args.max_results.clamp(1, SEARCH_MAX_RESULTS);
    let mut matches = Vec::new();
    let mut files_read = 0_usize;
    let mut truncated = false;
    let mut stack = vec![start];

    'walk: while let Some(path) = stack.pop() {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            let entries = match std::fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if is_dir && SEARCH_SKIP_DIRS.contains(&name.as_ref()) {
                    continue;
                }
                stack.push(entry.path());
            }
            continue;
        }
        if !metadata.is_file() || metadata.len() > SEARCH_MAX_FILE_BYTES {
            continue;
        }
        files_read += 1;
        if files_read > SEARCH_MAX_FILES {
            truncated = true;
            break;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let relative = path.strip_prefix(workspace.root()).unwrap_or(&path);
        for (index, line) in text.lines().enumerate() {
            let matched = if args.case_sensitive {
                line.contains(&needle)
            } else {
                line.to_lowercase().contains(&needle)
            };
            if matched {
                matches.push(format!(
                    "{}:{}: {}",
                    relative.display(),
                    index + 1,
                    truncate_line(line)
                ));
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
    workspace: LocalWorkspace,
}

impl EditFileTool {
    pub fn new(workspace: LocalWorkspace) -> Self {
        Self { workspace }
    }

    fn replacement(&self, args: &EditFileArgs) -> Result<(String, String, usize), ToolError> {
        if args.old_string.is_empty() {
            return Err(ToolError::new("old_string must not be empty"));
        }
        if args.old_string == args.new_string {
            return Err(ToolError::new(
                "old_string and new_string are identical; there is nothing to change",
            ));
        }
        let path = self.workspace.resolve(&args.path)?;
        let metadata = std::fs::metadata(&path)
            .map_err(|error| ToolError::new(format!("cannot read `{}`: {error}", args.path)))?;
        if !metadata.is_file() {
            return Err(ToolError::new(format!("`{}` is not a file", args.path)));
        }
        if metadata.len() > EDIT_FILE_MAX_BYTES as u64 {
            return Err(ToolError::new(format!(
                "`{}` exceeds {EDIT_FILE_MAX_BYTES} bytes",
                args.path
            )));
        }
        let content = std::fs::read_to_string(&path).map_err(|error| {
            ToolError::new(format!("cannot read `{}` as UTF-8: {error}", args.path))
        })?;
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
        Some(match self.replacement(&args) {
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
        let (old, new, count) = self.replacement(&args)?;
        let workspace = self.workspace.clone();
        let path = args.path.clone();
        let old_for_write = old.clone();
        let new_for_write = new.clone();
        run_blocking(move || {
            workspace.write_atomic_if_unchanged(&path, &old_for_write, &new_for_write)
        })
        .await?;
        Ok(format!(
            "replaced {count} occurrence(s) in {}\n{}",
            args.path,
            unified_preview(&args.path, &old, &new, PREVIEW_MAX_BYTES)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs2::FileExt;

    #[test]
    fn the_workspace_file_lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = LocalWorkspace::open(dir.path()).unwrap();
        let first = workspace.lock_workspace().unwrap();
        let path = dir.path().join(".autra/workspace.lock");
        let second = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();

        let blocked = second.try_lock_exclusive();
        assert!(blocked.is_err());

        drop(first);
        second.try_lock_exclusive().unwrap();
    }
}
