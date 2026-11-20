# 01: ContextBuilder and Skills

**What to build:** A developer can configure how model context is assembled, keep old observations within a character budget without deleting execution facts, and expose reusable instructions that the model loads by name.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] `ContextBuilder` is a public harness seam; `SimpleContextBuilder` is the default.
- [x] The simple builder assembles instructions, history and tools and lists available skills.
- [x] A character budget truncates old Tool/Assistant content with a marker, never removing items or tool-call ids.
- [x] `Skill` and `SkillRegistry` index named instructions; `load_skill` returns them.
- [x] An Agent with skills registers `load_skill`; loading does not widen permissions.
- [x] Runtime and harness tests cover the skill index, budget truncation and an end-to-end load.
- [x] Docs and an ADR record the compaction boundary.
