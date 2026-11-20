use std::fs;

use autra_core::EffectClass;
use autra_harness::{EditFileTool, LocalWorkspace, Tool};
use tempfile::tempdir;

fn tool(dir: &std::path::Path) -> EditFileTool {
    EditFileTool::new(LocalWorkspace::open(dir).unwrap())
}

#[tokio::test]
async fn edit_file_replaces_a_unique_string_and_returns_a_diff() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("lib.rs"), "let value = 1;\n").unwrap();
    let tool = tool(dir.path());

    let output = tool
        .call(r#"{"path":"lib.rs","old_string":"1","new_string":"2"}"#)
        .await
        .unwrap();

    assert!(tool.requires_approval());
    assert_eq!(tool.effect_class(), EffectClass::Idempotent);
    assert_eq!(
        fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        "let value = 2;\n"
    );
    assert!(output.contains("replaced 1 occurrence(s)"));
    assert!(output.contains("-let value = 1;"));
    assert!(output.contains("+let value = 2;"));
}

#[tokio::test]
async fn edit_file_rejects_a_missing_old_string() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("lib.rs"), "hello").unwrap();

    let error = tool(dir.path())
        .call(r#"{"path":"lib.rs","old_string":"absent","new_string":"x"}"#)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("not found"));
    assert_eq!(
        fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        "hello"
    );
}

#[tokio::test]
async fn edit_file_rejects_ambiguous_matches_unless_replace_all() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("lib.rs"), "x\nx\n").unwrap();
    let tool = tool(dir.path());

    let error = tool
        .call(r#"{"path":"lib.rs","old_string":"x","new_string":"y"}"#)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("matches 2 times"));

    let output = tool
        .call(r#"{"path":"lib.rs","old_string":"x","new_string":"y","replace_all":true}"#)
        .await
        .unwrap();
    assert!(output.contains("replaced 2 occurrence(s)"));
    assert_eq!(
        fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        "y\ny\n"
    );
}

#[tokio::test]
async fn edit_file_rejects_identical_strings() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("lib.rs"), "hello").unwrap();

    let error = tool(dir.path())
        .call(r#"{"path":"lib.rs","old_string":"hello","new_string":"hello"}"#)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("nothing to change"));
}

#[tokio::test]
async fn edit_file_preview_shows_the_diff_or_the_rejection() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("lib.rs"), "x\nx\n").unwrap();
    let tool = tool(dir.path());

    let ambiguous = tool
        .preview(r#"{"path":"lib.rs","old_string":"x","new_string":"y"}"#)
        .unwrap();
    assert!(ambiguous.contains("will be rejected"));
    assert!(ambiguous.contains("matches 2 times"));

    let clean = tool
        .preview(r#"{"path":"lib.rs","old_string":"x","new_string":"y","replace_all":true}"#)
        .unwrap();
    assert!(clean.contains("-x"));
    assert!(clean.contains("+y"));
}

#[tokio::test]
async fn edit_file_rejects_paths_outside_the_workspace() {
    let dir = tempdir().unwrap();

    let error = tool(dir.path())
        .call(r#"{"path":"../outside.txt","old_string":"a","new_string":"b"}"#)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("workspace"));
}
