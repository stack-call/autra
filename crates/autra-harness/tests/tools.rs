use autra_core::EffectClass;
use autra_harness::{EchoTool, Tool, ToolRegistry};

#[tokio::test]
async fn echo_tool_returns_its_text_argument() {
    let output = EchoTool.call(r#"{"text":"hello"}"#).await.unwrap();

    assert_eq!(output, "hello");
    assert_eq!(EchoTool.effect_class(), EffectClass::ReadOnly);
}

#[tokio::test]
async fn echo_tool_rejects_malformed_arguments() {
    let error = EchoTool.call("{}").await.unwrap_err();

    assert!(error.to_string().contains("invalid echo arguments"));
}

#[test]
fn registry_exposes_registered_tool_specs() {
    let mut registry = ToolRegistry::new();
    registry.register(EchoTool);

    assert_eq!(registry.names(), ["echo"]);
    assert_eq!(registry.specs().len(), 1);
    assert!(registry.get("echo").is_some());
    assert!(registry.get("missing").is_none());
}
