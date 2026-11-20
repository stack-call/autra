//! Typed observer hooks. Observers see accepted execution and cannot change it.
//! Behaviour modifiers and Policy Authority are future, host-installed stages.

use autra_core::{AgentPhase, EffectIntent, RunId};

pub trait ObserverHook: Send + Sync {
    fn on_run_started(&self, _run_id: &RunId) {}
    fn on_model_request(&self, _run_id: &RunId, _turn: u32) {}
    fn on_effect_accepted(&self, _run_id: &RunId, _effect: &EffectIntent) {}
    fn on_run_finished(&self, _run_id: &RunId, _phase: &AgentPhase) {}
}
