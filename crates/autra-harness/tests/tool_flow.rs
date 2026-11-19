use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, AgentStepper, BudgetSnapshot, Disposition, EffectIntent,
    EventKind, HistoryItem, ModelOutput, RunId, ToolCallRequest, ToolOutcome, ToolSpec,
};
use autra_harness::ReActStepper;

fn echo_spec() -> ToolSpec {
    ToolSpec {
        name: "echo".to_owned(),
        description: "Echo".to_owned(),
        input_schema: "{}".to_owned(),
    }
}

fn call(id: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: id.to_owned(),
        name: "echo".to_owned(),
        arguments: "{}".to_owned(),
    }
}

fn outcome(id: &str, content: &str) -> ToolOutcome {
    ToolOutcome {
        call_id: id.to_owned(),
        name: "echo".to_owned(),
        content: content.to_owned(),
        is_error: false,
    }
}

fn started(seq: u64) -> AcceptedEvent {
    AcceptedEvent {
        seq,
        kind: EventKind::RunStarted {
            input: "go".to_owned(),
        },
    }
}

fn model_result(seq: u64, calls: Vec<ToolCallRequest>) -> AcceptedEvent {
    AcceptedEvent {
        seq,
        kind: EventKind::ModelResult(ModelOutput {
            text: None,
            tool_calls: calls,
        }),
    }
}

#[test]
fn model_tool_calls_become_tool_effects_and_are_joined() {
    let stepper = ReActStepper::new(None, vec![echo_spec()]);
    let state = AgentState::new(RunId::new("run-tools"), BudgetSnapshot::new(4, 4));
    let state = stepper.transition(&state, &started(1)).unwrap().next;

    let transition = stepper
        .transition(&state, &model_result(2, vec![call("c1"), call("c2")]))
        .unwrap();
    let state = transition.next;
    assert_eq!(state.phase, AgentPhase::AwaitingTool);
    assert_eq!(state.pending_tool_calls.len(), 2);
    assert_eq!(state.budget.tool_calls_used, 2);
    assert_eq!(transition.intents.len(), 2);
    assert!(matches!(transition.intents[0], EffectIntent::CallTool(_)));

    let transition = stepper
        .transition(
            &state,
            &AcceptedEvent {
                seq: 3,
                kind: EventKind::ToolResult(outcome("c1", "a")),
            },
        )
        .unwrap();
    let state = transition.next;
    assert_eq!(state.phase, AgentPhase::AwaitingTool);
    assert_eq!(state.pending_tool_calls.len(), 1);
    assert!(transition.intents.is_empty());

    let transition = stepper
        .transition(
            &state,
            &AcceptedEvent {
                seq: 4,
                kind: EventKind::ToolResult(outcome("c2", "b")),
            },
        )
        .unwrap();
    assert_eq!(transition.next.phase, AgentPhase::AwaitingModel);
    assert_eq!(transition.next.budget.turns_used, 2);
    match &transition.intents[0] {
        EffectIntent::CallModel(spec) => {
            let tool_items = spec
                .messages
                .iter()
                .filter(|item| matches!(item, HistoryItem::Tool(_)))
                .count();
            assert_eq!(tool_items, 2);
        }
        other => panic!("expected a model call, got {other:?}"),
    }
    assert_eq!(transition.disposition, Disposition::Continue);
}

#[test]
fn tool_budget_is_all_or_nothing() {
    let stepper = ReActStepper::new(None, vec![echo_spec()]);
    let state = AgentState::new(RunId::new("run-budget"), BudgetSnapshot::new(4, 1));
    let state = stepper.transition(&state, &started(1)).unwrap().next;

    let transition = stepper
        .transition(&state, &model_result(2, vec![call("c1"), call("c2")]))
        .unwrap();

    assert_eq!(transition.next.phase, AgentPhase::Failed);
    assert_eq!(transition.disposition, Disposition::Failed);
    assert!(transition.intents.is_empty());
    assert_eq!(transition.next.budget.tool_calls_used, 0);
}

#[test]
fn duplicate_call_ids_fail_the_run() {
    let stepper = ReActStepper::new(None, vec![echo_spec()]);
    let state = AgentState::new(RunId::new("run-dup"), BudgetSnapshot::new(4, 4));
    let state = stepper.transition(&state, &started(1)).unwrap().next;

    let transition = stepper
        .transition(&state, &model_result(2, vec![call("c1"), call("c1")]))
        .unwrap();

    assert_eq!(transition.next.phase, AgentPhase::Failed);
    assert_eq!(transition.disposition, Disposition::Failed);
    assert!(
        transition
            .next
            .last_error
            .as_deref()
            .unwrap()
            .contains("duplicate")
    );
}

#[test]
fn empty_call_ids_fail_the_run() {
    let stepper = ReActStepper::new(None, vec![echo_spec()]);
    let state = AgentState::new(RunId::new("run-empty"), BudgetSnapshot::new(4, 4));
    let state = stepper.transition(&state, &started(1)).unwrap().next;

    let transition = stepper
        .transition(&state, &model_result(2, vec![call("")]))
        .unwrap();

    assert_eq!(transition.next.phase, AgentPhase::Failed);
    assert_eq!(transition.disposition, Disposition::Failed);
}

#[test]
fn unknown_tool_result_fails_the_run() {
    let stepper = ReActStepper::new(None, vec![echo_spec()]);
    let state = AgentState::new(RunId::new("run-unknown"), BudgetSnapshot::new(4, 4));
    let state = stepper.transition(&state, &started(1)).unwrap().next;
    let state = stepper
        .transition(&state, &model_result(2, vec![call("c1")]))
        .unwrap()
        .next;

    let transition = stepper
        .transition(
            &state,
            &AcceptedEvent {
                seq: 3,
                kind: EventKind::ToolResult(outcome("other", "x")),
            },
        )
        .unwrap();

    assert_eq!(transition.next.phase, AgentPhase::Failed);
    assert_eq!(transition.disposition, Disposition::Failed);
    assert!(
        transition
            .next
            .last_error
            .as_deref()
            .unwrap()
            .contains("unknown call id")
    );
}
