# Context and Memory

## Three layers, kept separate

| Layer | Answers | Can be compacted? |
|---|---|---|
| Execution State / Journal | What happened and can the Run continue? | No; migrate by protocol |
| Context View | What does the model see next? | Yes, with provenance |
| Long-term Memory | What survives across tasks? | Yes, under retention policy |

The Context View is not a journal. Compacting it never changes execution facts.

## Current Context View

`ReActStepper` delegates to a `ContextBuilder`. The default
`SimpleContextBuilder` assembles:

- the Agent system instructions and available skill index;
- the accepted history: user, assistant (text and tool calls) and tool
  outcomes;
- the registered tool specs.

A character budget (`context_budget_chars`) counts the assembled system prompt
(including the skill index), tool schemas and accepted history. It truncates
tool results, assistant text and user text with a marker. If the fixed parts
cannot fit, or tool-call arguments alone exceed the budget, building the request
fails instead of sending an oversized prompt. It is a character proxy, not a
tokenizer. Skills are loaded through `load_skill`, which does not grant
permissions. Planning is a separate tool-backed checklist: `update_plan` replaces an
in-memory plan and `Agent::plan()` exposes it. There is no retrieval memory yet.

## Planned layers

- **ContextBuilder** with selection, compaction and provenance.
- **Skills**: reusable instructions loaded on demand; loading a Skill does not
  grant Tool permission.
- **Memory**: cross-session facts and retrieval; it cannot replace a Run
  checkpoint.
- **MCP**: tool discovery and invocation; it does not bypass local policy or
  Effect classification.

## Rules

- Never compact away an unresolved approval, a pending tool call id or an
  accepted execution fact. See INV-07.
- Every Context Record should carry `run_id`, `turn_id`, `model_effect_id`,
  `builder_revision`, `agent_revision`, `source_refs[]`,
  `visible_tools_hash`, `request_digest` and token budget.
- Treat repository content, web content and model output as untrusted data.
  Mark provenance in the prompt and enforce boundaries in the Tool Host.
- A summary is model input, not an immutable fact. Keep a traceable source for
  important conclusions.
