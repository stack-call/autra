use autra_harness::{
    EchoModel, FakeModel, MODEL_PROTOCOL_VERSION, ModelCapabilities, ModelProvider,
};

#[test]
fn echo_model_declares_default_capabilities() {
    assert_eq!(EchoModel.capabilities(), ModelCapabilities::default());
    assert_eq!(EchoModel.protocol_version(), MODEL_PROTOCOL_VERSION);
}

#[test]
fn fake_model_declares_tool_calling() {
    let model = FakeModel::new(Vec::new());

    assert!(model.capabilities().tool_calling);
    assert!(!model.capabilities().streaming);
    assert_eq!(model.protocol_version(), MODEL_PROTOCOL_VERSION);
}
