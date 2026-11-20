use autra_core::{AgentState, BudgetSnapshot, RunId};

#[test]
fn state_serializes_to_a_stable_snapshot() {
    let state = AgentState::new(RunId::new("run-snapshot"), BudgetSnapshot::new(2, 3));

    let value = serde_json::to_value(&state).unwrap();

    assert_eq!(
        value,
        serde_json::json!({
            "run_id": "run-snapshot",
            "session_id": null,
            "turn_id": null,
            "phase": "Ready",
            "history": [],
            "pending_tool_calls": [],
            "budget": {
                "max_turns": 2,
                "turns_used": 0,
                "max_tool_calls": 3,
                "tool_calls_used": 0
            },
            "final_answer": null,
            "last_error": null
        })
    );
}
