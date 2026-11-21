use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, AgentStepper, BudgetSnapshot, Disposition, EffectIntent,
    EngineState, EventKind, ModelOutput, RunId,
};
use autra_harness::{ReActStepper, react_history};

fn started(input: &str) -> AcceptedEvent {
    AcceptedEvent {
        seq: 1,
        kind: EventKind::RunStarted {
            input: input.to_owned(),
        },
    }
}

#[test]
fn run_started_requests_a_model_call() {
    let stepper = ReActStepper::new(Some("be brief".to_owned()), Vec::new());
    let state = AgentState::new(RunId::new("run-1"), BudgetSnapshot::new(4, 4));

    let transition = stepper.transition(&state, &started("hello")).unwrap();

    assert_eq!(transition.next.phase, AgentPhase::AwaitingModel);
    assert_eq!(transition.next.budget.turns_used, 1);
    assert_eq!(transition.disposition, Disposition::Continue);
    assert_eq!(react_history(&transition.next).unwrap().len(), 1);
    match &transition.intents[0] {
        EffectIntent::CallModel(spec) => {
            assert_eq!(spec.system.as_deref(), Some("be brief"));
            assert_eq!(spec.messages.len(), 1);
        }
        other => panic!("expected a model call, got {other:?}"),
    }
}

#[test]
fn final_model_result_completes_the_run() {
    let stepper = ReActStepper::new(None, Vec::new());
    let state = AgentState::new(RunId::new("run-2"), BudgetSnapshot::new(4, 4));
    let state = stepper.transition(&state, &started("hello")).unwrap().next;
    let event = AcceptedEvent {
        seq: 2,
        kind: EventKind::ModelResult(ModelOutput {
            usage: None,
            text: Some("done".to_owned()),
            tool_calls: Vec::new(),
        }),
    };

    let transition = stepper.transition(&state, &event).unwrap();

    assert_eq!(transition.next.phase, AgentPhase::Completed);
    assert_eq!(transition.next.final_answer.as_deref(), Some("done"));
    assert_eq!(react_history(&transition.next).unwrap().len(), 2);
    assert!(transition.intents.is_empty());
    assert_eq!(transition.disposition, Disposition::Finished);
}

#[test]
fn an_uninitialized_state_reads_as_empty_history() {
    let state = AgentState::new(RunId::new("fresh"), BudgetSnapshot::new(4, 4));

    assert!(react_history(&state).unwrap().is_empty());
}

#[test]
fn corrupt_react_state_is_rejected() {
    let mut state = AgentState::new(RunId::new("corrupt"), BudgetSnapshot::new(4, 4));
    state.engine_state =
        EngineState::new("react", 1).with_data(serde_json::json!({ "history": 3 }));

    let error = react_history(&state).unwrap_err();

    assert!(error.to_string().contains("cannot decode"));
}

#[test]
fn a_newer_react_schema_is_rejected() {
    let mut state = AgentState::new(RunId::new("future"), BudgetSnapshot::new(4, 4));
    state.engine_state = EngineState::new("react", 2).with_data(serde_json::json!({}));

    let error = react_history(&state).unwrap_err();

    assert!(error.to_string().contains("newer"));
}

#[test]
fn a_foreign_engine_state_is_rejected() {
    let mut state = AgentState::new(RunId::new("foreign"), BudgetSnapshot::new(4, 4));
    state.engine_state = EngineState::new("other", 1).with_data(serde_json::json!({}));

    let error = react_history(&state).unwrap_err();

    assert!(error.to_string().contains("belongs to"));
}
