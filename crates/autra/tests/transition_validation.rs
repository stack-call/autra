use async_trait::async_trait;
use autra::Agent;
use autra_core::{
    AcceptedEvent, AgentPhase, AgentState, AgentStepper, Disposition, EventKind, RunId, StepError,
    TransitionProposal,
};
use autra_harness::EchoModel;

#[derive(Clone, Copy)]
enum Violation {
    RunId,
    Parent,
    BudgetLimit,
    Phase,
}

struct BadStepper(Violation);

#[async_trait]
impl AgentStepper for BadStepper {
    fn transition(
        &self,
        state: &AgentState,
        event: &AcceptedEvent,
    ) -> Result<TransitionProposal, StepError> {
        let mut next = state.clone();
        if matches!(event.kind, EventKind::RunStarted { .. }) {
            next.phase = AgentPhase::AwaitingModel;
            match self.0 {
                Violation::RunId => next.run_id = RunId::new("other-run"),
                Violation::Parent => next.parent_run_id = Some(RunId::new("other-parent")),
                Violation::BudgetLimit => next.budget.max_turns += 1,
                Violation::Phase => next.phase = AgentPhase::Ready,
            }
        }
        Ok(TransitionProposal {
            next,
            intents: Vec::new(),
            disposition: Disposition::Continue,
            engine_delta: None,
        })
    }
}

async fn run_with(violation: Violation) -> String {
    let agent = Agent::builder("invalid-stepper")
        .model(EchoModel)
        .stepper(BadStepper(violation))
        .build()
        .unwrap();

    agent.run("go").await.unwrap_err().to_string()
}

#[tokio::test]
async fn a_transition_cannot_change_the_run_id() {
    assert!(
        run_with(Violation::RunId)
            .await
            .contains("must not change the run id")
    );
}

#[tokio::test]
async fn a_transition_cannot_change_the_parent_run_id() {
    assert!(
        run_with(Violation::Parent)
            .await
            .contains("must not change the parent run id")
    );
}

#[tokio::test]
async fn a_transition_cannot_raise_budget_limits() {
    assert!(
        run_with(Violation::BudgetLimit)
            .await
            .contains("must not change budget limits")
    );
}

#[tokio::test]
async fn a_transition_must_follow_the_phase_graph() {
    assert!(
        run_with(Violation::Phase)
            .await
            .contains("illegal phase transition")
    );
}

struct BadInitialize;

#[async_trait]
impl AgentStepper for BadInitialize {
    fn initialize(&self, state: &mut AgentState, _history: &[autra_core::HistoryItem]) {
        state.run_id = RunId::new("other-run");
    }

    fn transition(
        &self,
        state: &AgentState,
        _event: &AcceptedEvent,
    ) -> Result<TransitionProposal, StepError> {
        Ok(TransitionProposal {
            next: state.clone(),
            intents: Vec::new(),
            disposition: Disposition::Continue,
            engine_delta: None,
        })
    }
}

#[tokio::test]
async fn initialize_cannot_change_the_run_identity() {
    let agent = Agent::builder("invalid-initialize")
        .model(EchoModel)
        .stepper(BadInitialize)
        .build()
        .unwrap();

    let error = agent.run("go").await.unwrap_err().to_string();

    assert!(error.contains("initialize must not change the run id"));
}
