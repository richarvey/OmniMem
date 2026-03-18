## OmniMem — Persistent Semantic Memory

You have access to a persistent memory system via the `omnimem` MCP server.

### Session Start
At the beginning of every session:
1. Call `briefing(project="<project_name>")` — this single call aggregates project context, experience summary, stale memories, new knowledge articles, contradiction warnings, and reinstate candidates
2. Briefly summarise what you found: current state, recent decisions, any abandoned approaches to avoid, stale memories to review, and any contradictions that need resolution

### During a Session
- Before attempting to solve a non-trivial problem, call `recall("<problem description>")` — you may find a prior solution or a relevant article
- Before suggesting a library or architectural approach, call `warn_if_abandoned("<library or approach name>")`. If a warning comes back, tell the human before proceeding: *"We tried [X] before and abandoned it because [reason] — shall we try again or look for alternatives?"*
- When you and the human reach a decision, solve a tricky bug, discover a pattern, or agree on an approach: call `remember("<what was decided/discovered>", project="<project_name>", tags=["<relevant_tag>"])`
- If the human says something like "forget about X" or "stop bringing up Y" or "I don't do that anymore": call `deprioritise()` with a clear reason. Do NOT hard delete unless they say "permanently delete" or "wipe"
- If a recalled memory seems to keep coming back when it shouldn't, call `suppress_topic("<topic>")` and let the human know

### Recording Experience
After solving any non-trivial problem (or giving up on one), record the experience:

```
record_experience(
  key="mem:episodic:...",
  effort_score=3,           # 1=trivial, 5=battle-hardened
  outcome="succeeded",      # succeeded | pivoted | abandoned
  iterations=2,
  abandoned_approaches=[
    {"name": "onnxruntime", "type": "library", "reason": "SIGILL on Alpine musl libc"}
  ],
  breakthrough="sentence-transformers with --prefer-binary pip flag",
  gotchas=["Needs openblas-dev and g++ installed in Alpine first"]
)
```

**Bug fixes must always be recorded.** After fixing any bug, call `remember()` with a structured description covering:
- **Symptom**: What the user saw (error message, HTTP status, unexpected behaviour)
- **Cause**: What was actually wrong (missing dependency, wrong logic, etc.)
- **Fix**: What was changed to resolve it

This ensures future sessions can recall prior fixes when similar symptoms appear.

For dead ends discovered mid-session, use `log_abandoned(key, name, type, reason)` to record them incrementally as they happen.

If `effort_score >= 4` and `outcome == "abandoned"`, the system will automatically suppress the abandoned approach names — dead ends don't need to keep resurfacing.

**Effort score guide:**
- **1** — Worked first time, no issues
- **2** — Minor friction, quick fix
- **3** — Multiple iterations, some debugging
- **4** — Significant effort, approach changes required
- **5** — Near-abandonment, fundamental rethink

### Session End
When wrapping up:
1. Call `update_project_state("<project_name>", current_state="<current state>", notes="<anything important for next session>")`
2. If significant work was done, call `remember()` for any key outcomes not already stored
3. Ensure `record_experience()` has been called for any non-trivial work this session

### Key Principles
- Prefer `deprioritise` over `forget` — humans usually mean "stop surfacing this" not "destroy this"
- Always include a `reason` when deprioritising — it helps future sessions understand the context
- Include `reinstate_hints` when deprioritising if the memory might become relevant again
- When a recalled knowledge article seems relevant, mention it as a starting point: *"I found an article from [source] about X — shall I use that as a research base?"*
- The graveyard of abandoned approaches is as valuable as the list of successes — consult it before suggesting solutions
- Never store secrets, credentials, or personally sensitive data in memory
