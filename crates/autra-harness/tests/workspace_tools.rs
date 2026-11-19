use std::fs;
use std::path::Path;

use autra_core::EffectClass;
use autra_harness::{ListFilesTool, LocalWorkspace, ReadFileTool, Tool};
use tempfile::tempdir;

fn workspace(root: &Path) -> LocalWorkspace {
    LocalWorkspace::open(root).unwrap()
}

#[tokio::test]
async fn read_file_returns_workspace_content() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "hello").unwrap();
    let tool = ReadFileTool::new(workspace(dir.path()));

    let output = tool.call(r#"{"path":"note.txt"}"#).await.unwrap();

    assert_eq!(output, "hello");
    assert_eq!(tool.effect_class(), EffectClass::ReadOnly);
}

#[tokio::test]
async fn read_file_rejects_absolute_and_parent_paths() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "hello").unwrap();
    let tool = ReadFileTool::new(workspace(dir.path()));

    let absolute =
        serde_json::json!({ "path": dir.path().join("note.txt").to_string_lossy() }).to_string();
    let absolute_error = tool.call(&absolute).await.unwrap_err();
    assert!(absolute_error.to_string().contains("absolute"));

    let parent_error = tool.call(r#"{"path":"../outside.txt"}"#).await.unwrap_err();
    assert!(parent_error.to_string().contains("workspace"));
}

#[cfg(unix)]
fn make_symlink(original: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(original, link)
}

#[cfg(windows)]
fn make_symlink(original: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(original, link)
}

#[tokio::test]
async fn read_file_rejects_symlink_escapes() {
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let secret = outside.path().join("secret.txt");
    fs::write(&secret, "secret").unwrap();
    let link = dir.path().join("link.txt");
    if let Err(error) = make_symlink(&secret, &link) {
        eprintln!("skipping symlink escape test: cannot create symlink ({error})");
        return;
    }
    let tool = ReadFileTool::new(workspace(dir.path()));

    let error = tool.call(r#"{"path":"link.txt"}"#).await.unwrap_err();

    assert!(error.to_string().contains("workspace"));
}

#[tokio::test]
async fn read_file_marks_truncation() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "0123456789abcdef").unwrap();
    let tool = ReadFileTool::new(workspace(dir.path())).with_max_bytes(10);

    let output = tool.call(r#"{"path":"note.txt"}"#).await.unwrap();

    assert!(output.starts_with("0123456789"));
    assert!(output.contains("truncated at 10 bytes"));
}

#[tokio::test]
async fn list_files_sorts_and_marks_directories() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("b.txt"), "").unwrap();
    fs::write(dir.path().join("a.txt"), "").unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    let tool = ListFilesTool::new(workspace(dir.path()));

    let output = tool.call("{}").await.unwrap();

    assert_eq!(output, "a.txt\nb.txt\nsub/");
    assert_eq!(tool.effect_class(), EffectClass::ReadOnly);
}

#[tokio::test]
async fn list_files_caps_entries() {
    let dir = tempdir().unwrap();
    for index in 0..205 {
        fs::write(dir.path().join(format!("file-{index:03}.txt")), "").unwrap();
    }
    let tool = ListFilesTool::new(workspace(dir.path()));

    let output = tool.call("{}").await.unwrap();

    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines.len(), 201);
    assert_eq!(lines[0], "file-000.txt");
    assert!(lines[200].contains("truncated at 200 entries"));
}

#[tokio::test]
async fn read_file_defaults_to_32_kib() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("big.txt"), "a".repeat(33 * 1024)).unwrap();
    let tool = ReadFileTool::new(workspace(dir.path()));

    let output = tool.call(r#"{"path":"big.txt"}"#).await.unwrap();

    assert!(output.contains("truncated at 32768 bytes"));
}

#[tokio::test]
async fn read_file_truncates_on_a_char_boundary() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("text.txt"), "aébc").unwrap();
    let tool = ReadFileTool::new(workspace(dir.path())).with_max_bytes(2);

    let output = tool.call(r#"{"path":"text.txt"}"#).await.unwrap();

    assert!(output.starts_with('a'));
    assert!(!output.contains('\u{fffd}'));
    assert!(output.contains("truncated at 2 bytes"));
}
