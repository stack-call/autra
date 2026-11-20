# The Outbox Loop Claims Before Dispatch

**Status:** accepted

The Dispatcher boundary existed, but nothing consumed `effect_outbox` rows: an
Authorized effect could sit enqueued forever, or two executors could run the
same intent. The outbox v0 had enqueue, claim, release and complete operations;
the claim-driven loop that uses them was missing.

`OutboxDispatcher::run_once_for_run` now:

1. claims due rows for one Run through `claim_due_effects_for_run`, which stamps
   `claim_id` and a lease deadline and increments `attempts`;
2. decodes `EffectRecord::intent_json` into an `EffectIntent`;
3. calls `Dispatcher::call_model` or `Dispatcher::call_tool`;
4. maps the outcome onto the EffectRecord (Committed, Failed or
   OutcomeUnknown) and records one `EffectAttempt`;
5. completes the outbox row so it is not dispatched again.

If the intent cannot be decoded, the record is missing, or recording the
outcome fails, the code releases the claim instead of completing the row, so a
later pass can retry. Executions are tracked in `DispatchResult` without
changing the Agent's in-process event flow.

Consequences: dispatch has one claim-first path, and the outbox and the effect
ledger cannot disagree about whether a claimed effect completed. The Agent's
own execution path still calls the Dispatcher directly and records its own
effects; wiring the Agent to enqueue and use this loop, plus lease expiry retry
and resume, remains pending. Claiming stops duplicate concurrent execution, not
duplicate external side effects after a crash between the call and the
outcome commit; that remains OutcomeUnknown territory.

Alternatives considered: dispatch directly from `enqueue_effect` (no lease, no
retry), and a background worker thread (premature before the Agent path is
migrated).
