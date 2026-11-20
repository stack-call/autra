use std::fs;
use std::path::Path;

use autra_core::EffectClass;
use autra_harness::{LocalWorkspace, Tool, WriteFileTool};
use tempfile::tempdir;

fn workspace(root: &Path) -> LocalWorkspace {
    LocalWorkspace::open(root).unwrap()
}

#[tokio::test]
async fn write_file_requires_approval_and_writes_content() {
    let dir = tempdir().unwrap();
    let tool = WriteFileTool::new(workspace(dir.path()));

    let output = tool
        .call(r#"{"path":"note.txt","content":"hello"}"#)
        .await
        .unwrap();

    assert!(tool.requires_approval());
    assert_eq!(tool.effect_class(), EffectClass::Idempotent);
    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "hello"
    );
    assert!(output.contains("wrote 5 bytes"));
}

#[tokio::test]
async fn write_file_overwrites_existing_content() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "old").unwrap();
    let tool = WriteFileTool::new(workspace(dir.path()));

    tool.call(r#"{"path":"note.txt","content":"new"}"#)
        .await
        .unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "new"
    );
}

#[tokio::test]
async fn write_file_rejects_paths_outside_the_workspace() {
    let dir = tempdir().unwrap();
    let tool = WriteFileTool::new(workspace(dir.path()));

    let absolute = serde_json::json!({
        "path": dir.path().join("note.txt").to_string_lossy(),
        "content": "x"
    })
    .to_string();
    let absolute_error = tool.call(&absolute).await.unwrap_err();
    assert!(absolute_error.to_string().contains("absolute"));

    let parent_error = tool
        .call(r#"{"path":"../outside.txt","content":"x"}"#)
        .await
        .unwrap_err();
    assert!(parent_error.to_string().contains("workspace"));
}

#[tokio::test]
async fn write_file_caps_content() {
    let dir = tempdir().unwrap();
    let tool = WriteFileTool::new(workspace(dir.path()));
    let content = "a".repeat(1024 * 1024 + 1);
    let arguments = serde_json::json!({ "path": "big.txt", "content": content }).to_string();

    let error = tool.call(&arguments).await.unwrap_err();

    assert!(error.to_string().contains("exceeds"));
    assert!(!dir.path().join("big.txt").exists());
}

#[tokio::test]
async fn write_file_creates_missing_parent_directories() {
    let dir = tempdir().unwrap();
    let tool = WriteFileTool::new(workspace(dir.path()));

    tool.call(r#"{"path":"src/nested/note.txt","content":"hello"}"#)
        .await
        .unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join("src/nested/note.txt")).unwrap(),
        "hello"
    );
}
