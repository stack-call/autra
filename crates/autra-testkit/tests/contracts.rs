use autra_harness::{EchoModel, EchoTool, ReActStepper};
use autra_runtime::LocalHost;
use autra_testkit::{check_host, check_model, check_stepper, check_tool};

#[test]
fn built_in_extensions_pass_protocol_contracts() {
    check_tool(&EchoTool).assert_ok();
    check_model(&EchoModel).assert_ok();
    check_stepper(&ReActStepper::new(None, Vec::new())).assert_ok();
    check_host(&LocalHost).assert_ok();
}
