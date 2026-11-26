use autra_execution::ToolError;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
static WRITE_LOCK: Mutex<()> = Mutex::new(());

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
