# 01: Session and multiple turns

**What to build:** A developer can hold a conversation with one Agent across several turns. Each turn sees the previous accepted messages, and the CLI can run a simple line-oriented chat without rebuilding state.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] `Session` owns an Agent and its accepted history; `run_turn` requires `&mut self`.
- [x] A turn seeds the previous history into a fresh Run with fresh budget and Run id.
- [x] Completed and Cancelled turns store the outcome history; errors leave it unchanged.
- [x] `SessionId` is unique and `turn_index` advances per turn.
- [x] `Agent::run_with_history` is available as the history-seeded seam.
- [x] `autractl chat [--model <name>]` runs one turn per stdin line.
- [x] Tests cover history seeding, session identity and the CLI chat.
