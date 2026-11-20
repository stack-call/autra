use autra_harness::EchoModel;
use autra_runtime::Agent;
use tracing_test::traced_test;

#[tokio::test]
#[traced_test]
async fn run_emits_a_trace_event() {
    let agent = Agent::builder("trace").model(EchoModel).build().unwrap();

    agent.run("hello").await.unwrap();

    assert!(logs_contain("agent run started"));
}
