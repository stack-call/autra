use std::fs;

use autra_core::EffectClass;
use autra_harness::{LocalWorkspace, SearchTextTool, Tool};
use tempfile::tempdir;

#[tokio::test]
async fn search_text_finds_matches_across_nested_files() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "let NEEDLE = 1;\n").unwrap();
    fs::write(dir.path().join("README.md"), "a needle in the docs\n").unwrap();
    let tool = SearchTextTool::new(LocalWorkspace::open(dir.path()).unwrap());

    let output = tool.call(r#"{"query":"needle"}"#).await.unwrap();

    assert_eq!(tool.effect_class(), EffectClass::ReadOnly);
    assert!(output.contains("README.md:1: a needle in the docs"));
    assert!(output.contains("lib.rs:1: let NEEDLE = 1;"));
}

#[tokio::test]
async fn search_text_is_case_sensitive_on_request() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "Needle\nneedle\n").unwrap();
    let tool = SearchTextTool::new(LocalWorkspace::open(dir.path()).unwrap());

    let output = tool
        .call(r#"{"query":"Needle","case_sensitive":true}"#)
        .await
        .unwrap();

    assert!(output.contains("note.txt:1: Needle"));
    assert!(!output.contains("note.txt:2: needle"));
}

#[tokio::test]
async fn search_text_skips_build_directories_and_caps_results() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("target")).unwrap();
    fs::write(dir.path().join("target/hidden.txt"), "needle\n").unwrap();
    for index in 0..3 {
        fs::write(dir.path().join(format!("file-{index}.txt")), "needle\n").unwrap();
    }
    let tool = SearchTextTool::new(LocalWorkspace::open(dir.path()).unwrap());

    let output = tool
        .call(r#"{"query":"needle","max_results":2}"#)
        .await
        .unwrap();

    assert!(!output.contains("target"));
    assert_eq!(output.matches("needle").count(), 2);
    assert!(output.contains("stopped at 2 matches"));
}

#[tokio::test]
async fn search_text_reports_no_matches() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("note.txt"), "hello").unwrap();
    let tool = SearchTextTool::new(LocalWorkspace::open(dir.path()).unwrap());

    let output = tool.call(r#"{"query":"absent"}"#).await.unwrap();

    assert_eq!(output, "no matches for `absent`");
}

#[tokio::test]
async fn search_text_rejects_paths_outside_the_workspace() {
    let dir = tempdir().unwrap();
    let tool = SearchTextTool::new(LocalWorkspace::open(dir.path()).unwrap());

    let error = tool
        .call(r#"{"query":"x","path":"../outside"}"#)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("workspace"));
}
