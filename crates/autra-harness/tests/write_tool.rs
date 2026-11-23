use std::fs;
use std::path::Path;

use autra_core::EffectClass;
use autra_harness::{LocalWorkspace, Tool, WriteFileTool};
use tempfile::tempdir;

#[test]
fn an_atomic_write_rejects_a_stale_pre_image() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("note.txt"), "current").unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();

    let error = workspace
        .write_atomic_if_unchanged("note.txt", "stale", "new")
        .unwrap_err();

    assert!(error.to_string().contains("changed since it was read"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "current"
    );
}

#[test]
fn an_atomic_write_replaces_content_without_leaving_temp_files() {
    let dir = tempdir().unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();

    workspace.write_atomic("note.txt", "first").unwrap();
    workspace.write_atomic("note.txt", "second").unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
        "second"
    );
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".autra-write-")
        })
        .collect();
    assert!(leftovers.is_empty(), "temporary files were not cleaned up");
}

#[cfg(unix)]
#[test]
fn an_atomic_write_refuses_a_symlink() {
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let target = outside.path().join("outside.txt");
    std::fs::write(&target, "outside").unwrap();
    std::os::unix::fs::symlink(&target, dir.path().join("link.txt")).unwrap();
    let workspace = LocalWorkspace::open(dir.path()).unwrap();

    let error = workspace.write_atomic("link.txt", "changed").unwrap_err();

    assert!(error.to_string().contains("symlink"));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "outside");
}

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

#[tokio::test]
async fn write_file_preview_marks_new_files_and_diffs_overwrites() {
    let dir = tempdir().unwrap();
    let tool = WriteFileTool::new(workspace(dir.path()));

    let creating = tool
        .preview(r#"{"path":"new.txt","content":"hello\nworld"}"#)
        .unwrap();
    assert!(creating.contains("create new.txt"));
    assert!(creating.contains("hello"));

    fs::write(dir.path().join("new.txt"), "old\n").unwrap();
    let overwriting = tool
        .preview(r#"{"path":"new.txt","content":"new\n"}"#)
        .unwrap();
    assert!(overwriting.contains("-old"));
    assert!(overwriting.contains("+new"));
}
