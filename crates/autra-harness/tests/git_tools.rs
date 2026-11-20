use std::fs;
use std::path::Path;
use std::process::Command;

use autra_core::EffectClass;
use autra_harness::{GitDiffTool, GitStatusTool, LocalWorkspace, Tool};
use tempfile::tempdir;

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git should run")
}

fn init_repo(dir: &Path) {
    let output = git(dir, &["init", "-q"]);
    assert!(output.status.success(), "git init failed");
}

fn commit_all(dir: &Path) {
    git(dir, &["add", "."]);
    let output = git(
        dir,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Autra Test",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    assert!(output.status.success(), "git commit failed");
}

#[tokio::test]
async fn git_status_reports_workspace_changes() {
    if !git_available() {
        eprintln!("skipping git test: git is not installed");
        return;
    }
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    fs::write(dir.path().join("note.txt"), "hello\n").unwrap();
    let tool = GitStatusTool::new(LocalWorkspace::open(dir.path()).unwrap());

    let output = tool.call("{}").await.unwrap();

    assert_eq!(tool.effect_class(), EffectClass::ReadOnly);
    assert!(output.contains("note.txt"));
}

#[tokio::test]
async fn git_diff_shows_unstaged_and_staged_changes() {
    if !git_available() {
        eprintln!("skipping git test: git is not installed");
        return;
    }
    let dir = tempdir().unwrap();
    init_repo(dir.path());
    fs::write(dir.path().join("note.txt"), "one\n").unwrap();
    commit_all(dir.path());
    fs::write(dir.path().join("note.txt"), "two\n").unwrap();
    let tool = GitDiffTool::new(LocalWorkspace::open(dir.path()).unwrap());

    let unstaged = tool.call("{}").await.unwrap();
    assert!(unstaged.contains("-one"));
    assert!(unstaged.contains("+two"));

    git(dir.path(), &["add", "note.txt"]);
    let staged = tool.call(r#"{"staged":true}"#).await.unwrap();
    assert!(staged.contains("-one"));
    assert!(staged.contains("+two"));

    let after_staged = tool.call("{}").await.unwrap();
    assert_eq!(after_staged, "(no unstaged changes)");
}

#[tokio::test]
async fn git_diff_rejects_paths_outside_the_workspace() {
    let dir = tempdir().unwrap();
    let tool = GitDiffTool::new(LocalWorkspace::open(dir.path()).unwrap());

    let error = tool.call(r#"{"path":"../outside"}"#).await.unwrap_err();

    assert!(error.to_string().contains("workspace"));
}
