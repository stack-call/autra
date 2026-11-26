use async_trait::async_trait;
use autra_execution::{
    DirectoryEntry, EnvironmentCapabilities, EnvironmentError, ExecutionEnvironment, FileKind,
    FileMetadata, FileRead,
};
use autra_harness::{
    EditFileTool, ListFilesTool, ReadFileTool, SearchTextTool, Tool, WriteFileTool,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
struct RemoteFiles(Mutex<BTreeMap<String, String>>);
#[async_trait]
impl ExecutionEnvironment for RemoteFiles {
    fn id(&self) -> &str {
        "remote:memory"
    }
    fn capabilities(&self) -> EnvironmentCapabilities {
        EnvironmentCapabilities {
            file_api: true,
            ..Default::default()
        }
    }
    async fn metadata(&self, path: &str) -> Result<FileMetadata, EnvironmentError> {
        if path == "." {
            return Ok(FileMetadata {
                kind: FileKind::Directory,
                len: 0,
            });
        }
        let path = path.strip_prefix("./").unwrap_or(path);
        self.0
            .lock()
            .unwrap()
            .get(path)
            .map(|s| FileMetadata {
                kind: FileKind::File,
                len: s.len() as u64,
            })
            .ok_or_else(|| EnvironmentError::Failed("missing".into()))
    }
    async fn read_file(&self, path: &str, limit: usize) -> Result<FileRead, EnvironmentError> {
        let files = self.0.lock().unwrap();
        let path = path.strip_prefix("./").unwrap_or(path);
        let text = files
            .get(path)
            .ok_or_else(|| EnvironmentError::Failed("missing".into()))?;
        Ok(FileRead {
            bytes: text.as_bytes().iter().copied().take(limit + 1).collect(),
            truncated: text.len() > limit,
        })
    }
    async fn list_directory(&self, _: &str) -> Result<Vec<DirectoryEntry>, EnvironmentError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .keys()
            .map(|name| DirectoryEntry {
                name: name.clone(),
                kind: FileKind::File,
            })
            .collect())
    }
    async fn write_file(&self, path: &str, content: &str) -> Result<(), EnvironmentError> {
        self.0.lock().unwrap().insert(path.into(), content.into());
        Ok(())
    }
    async fn write_file_if_unchanged(
        &self,
        path: &str,
        expected: &str,
        content: &str,
    ) -> Result<(), EnvironmentError> {
        let mut files = self.0.lock().unwrap();
        if files.get(path).map(String::as_str) != Some(expected) {
            return Err(EnvironmentError::Failed("stale content".into()));
        }
        files.insert(path.into(), content.into());
        Ok(())
    }
}
#[tokio::test]
async fn workspace_tools_use_the_same_remote_resource_contract() {
    let env = Arc::new(RemoteFiles(Mutex::new(BTreeMap::new())));
    WriteFileTool::in_environment(env.clone())
        .call(r#"{"path":"remote-only.txt","content":"hello"}"#)
        .await
        .unwrap();
    assert_eq!(
        ReadFileTool::in_environment(env.clone())
            .call(r#"{"path":"remote-only.txt"}"#)
            .await
            .unwrap(),
        "hello"
    );
    assert_eq!(
        ListFilesTool::in_environment(env.clone())
            .call("{}")
            .await
            .unwrap(),
        "remote-only.txt"
    );
    EditFileTool::in_environment(env.clone())
        .call(r#"{"path":"remote-only.txt","old_string":"hello","new_string":"world"}"#)
        .await
        .unwrap();
    assert_eq!(
        SearchTextTool::in_environment(env.clone())
            .call(r#"{"query":"world"}"#)
            .await
            .unwrap(),
        "remote-only.txt:1: world"
    );
    assert!(
        env.write_file_if_unchanged("remote-only.txt", "hello", "stale")
            .await
            .is_err()
    );
    assert_eq!(env.0.lock().unwrap()["remote-only.txt"], "world");
}
struct Unsupported;
#[async_trait]
impl ExecutionEnvironment for Unsupported {
    fn id(&self) -> &str {
        "unsupported"
    }
    fn capabilities(&self) -> EnvironmentCapabilities {
        EnvironmentCapabilities::default()
    }
}
#[tokio::test]
async fn unsupported_resources_never_fall_back_to_local_files() {
    assert!(
        ReadFileTool::in_environment(Arc::new(Unsupported))
            .call(r#"{"path":"Cargo.toml"}"#)
            .await
            .unwrap_err()
            .message
            .contains("unsupported environment capability")
    );
}
