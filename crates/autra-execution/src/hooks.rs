//! Typed observer hooks. Observers see accepted execution and cannot change it.
//! Behaviour modifiers and Policy Authority are future, host-installed stages.

use autra_core::{AgentPhase, EffectIntent, EffectRecord, RunId};

pub trait ObserverHook: Send + Sync {
    fn on_run_started(&self, _run_id: &RunId) {}
    fn on_model_request(&self, _run_id: &RunId, _turn: u32) {}

    /// Compatibility callback for implementations written before the proposed
    /// and committed stages were split. New code should implement
    /// [`ObserverHook::on_effect_proposed`].
    fn on_effect_accepted(&self, _run_id: &RunId, _effect: &EffectIntent) {}

    /// The stepper proposed an Effect. It is not authorized or committed yet.
    fn on_effect_proposed(&self, run_id: &RunId, effect: &EffectIntent) {
        self.on_effect_accepted(run_id, effect);
    }

    /// The Effect record is part of the authoritative run state. With a journal
    /// this fires only after the transaction succeeds. It does not mean the
    /// external call happened.
    fn on_effect_committed(&self, _run_id: &RunId, _effect: &EffectRecord) {}

    /// The authorization gate allowed a tool Effect to be dispatched.
    fn on_effect_authorized(&self, _run_id: &RunId, _effect: &EffectRecord) {}

    /// Policy or approval rejected a tool Effect.
    fn on_effect_rejected(&self, _run_id: &RunId, _effect: &EffectRecord) {}

    /// The Effect result is recorded, including `OutcomeUnknown`.
    fn on_effect_resolved(&self, _run_id: &RunId, _effect: &EffectRecord) {}

    fn on_run_finished(&self, _run_id: &RunId, _phase: &AgentPhase) {}
}
