# 01: Basic evaluation

**What to build:** A developer can run a list of tasks against an Agent and get a pass/fail report with answers and errors.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] `autra-eval` exposes EvalTask, EvalResult, EvalReport and run_tasks.
- [x] A task passes only when the answer contains the expected text.
- [x] Run errors are recorded as failed results with the error message.
- [x] The report exposes passed/failed counts and pass rate.
- [x] Deterministic Echo/Fake model tests cover pass, fail and error.
- [x] Docs and an ADR distinguish outcome evaluation from runtime correctness.
