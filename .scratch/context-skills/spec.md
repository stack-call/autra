# Context management and Skills

**Status:** ready-for-agent

**Source:** design spec §8 (Context engineering), §7.2 (Skills) and M5.

## Problem Statement

`ReActStepper` always sends the full history with the instructions and tool
specs. There is no ContextBuilder seam, no budget, and no way to load reusable
instructions on demand.

## Solution

Introduce a `ContextBuilder` seam with a `SimpleContextBuilder` that adds a
skill index to the system prompt and can truncate oversized message content
under a character budget. Add Skills as named instruction bundles loaded through
a `load_skill` tool.

## Implementation Decisions

- `ContextBuilder::build(ContextRequest)` returns a `ModelCallSpec` or a
  `ContextError`. `ReActStepper::with_context_builder` installs one; the default
  is `SimpleContextBuilder`.
- `SimpleContextBuilder` includes a system section listing available skills.
  It never changes the tool list.
- A character budget (`with_max_chars`) truncates the content of the oldest
  Tool and Assistant items with a marker. It never removes an item, a tool call
  id or a tool outcome, so execution facts and pairing survive.
- `Skill` has a name, description and instructions. `SkillRegistry` indexes
  them; `LoadSkillTool` returns the instructions for a requested name and is
  read-only.
- An Agent with skills automatically registers `load_skill`. Loading a skill
  does not grant tool permissions.
- No tokenizer dependency: the budget is a character proxy, documented as such.
- No automatic context persistence or long-term memory.

## Testing Decisions

- Harness: builder includes the skill index; budget truncates old content but
  keeps Tool items and user messages.
- Runtime: an Agent with a skill can call `load_skill` and see the instructions;
  the final answer completes.
- Existing ReAct, tool, approval and session tests stay green.

## Out of Scope

Tokenizer-accurate budgets, automatic summarization, retrieval memory, MCP,
per-skill permissions and durable context snapshots.
