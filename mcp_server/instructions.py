"""MCP server instructions delivered to agents via the protocol's instructions field.

This is the embedded copy of the usage guide. The human-readable source lives at
claude_config/CLAUDE.md — keep them in sync when making changes.
"""

INSTRUCTIONS = """\
## OmniMem — Persistent Semantic Memory

You have access to a persistent memory system via the `omnimem` MCP server.

-----

### Session Start

At the beginning of every session:

1. **Determine the project name** — use the current working directory name as the default. If uncertain, call `list_projects()` and match against known projects. If the project is genuinely ambiguous, ask the human before proceeding.
1. Call `briefing(project="<project_name>")` — this single call aggregates project context, experience summary, stale memories, new knowledge articles, contradiction warnings, and reinstate candidates.
1. If `briefing()` returns no project context but the project has episodic memories, call `compile_project_context("<project_name>", auto_save=True)` to auto-generate the context from stored memories. Review the compiled draft with the human and refine via `set_project_context()` if needed. If there are no episodic memories either (genuinely new project), call:

   ```
   set_project_context(
     name="<project_name>",
     description="<ask the human for a brief description>",
     stack=["<technologies>"],
     goals=["<current goals>"],
     current_state="<starting point>"
   )
   ```
1. Briefly summarise what you found:
- Current project state and goals
- Recent decisions and discovered patterns
- Abandoned approaches to avoid (graveyard)
- Stale memories that may need reviewing
- **Contradiction warnings** — surface these explicitly and ask the human which version reflects current reality before proceeding with any work
1. If the MCP server seems unresponsive or recall is slow, call `health()` and report the status to the human before continuing.

-----

### During a Session

**Before attempting any problem** where you would normally reach for documentation or a search engine:

- Call `recall("<problem description>")` — you may find a prior solution, a relevant pattern, or a knowledge article that gives you a head start
- If a recalled knowledge article seems relevant, mention it: *"I found an article from [source] about X — shall I use that as a research base?"*

**Before suggesting OR agreeing to any library, tool, or architectural approach** — including ones the human proposes:

- Call `warn_if_abandoned("<library or approach name>")`
- If a warning comes back, tell the human before proceeding: *"We tried [X] before and abandoned it because [reason] — shall we try again or look for alternatives?"*
- Do not skip this check because the human suggested the approach. Dead ends are dead ends regardless of who proposed them.

**When storing memories**, call `remember()` when you and the human:

- Reach a decision or agree on an approach
- Solve a tricky or non-obvious bug
- Discover a pattern, constraint, or gotcha
- Complete a meaningful piece of work

Use this tagging vocabulary for consistency:

|Category|Example tags                                                        |
|--------|--------------------------------------------------------------------|
|Stack   |`rust`, `python`, `docker`, `traefik`, `valkey`, `n8n`              |
|Intent  |`bug-fix`, `decision`, `pattern`, `gotcha`, `config`, `architecture`|
|Outcome |`working`, `deprecated`, `revisit`                                  |
|Project |use the project name as a tag                                       |

Always include at least one stack tag and one intent tag.

**If the human says** something like "forget about X", "stop bringing up Y", or "I don't do that anymore":

- Call `deprioritise()` with a clear reason
- Add `reinstate_hints` if the memory might become relevant again in a different context
- Do NOT hard delete unless they say "permanently delete" or "wipe"

**If a recalled memory keeps surfacing when it clearly shouldn't:**

- Call `suppress_topic("<topic>")` and let the human know it has been suppressed

**Periodic maintenance** — after every 10 `remember()` calls in a session, or at the end of any long session:

- Call `find_duplicates()` on the active namespace and flag any clusters to the human
- Call `check_contradictions()` on recent memories and surface any conflicts for resolution

-----

### Recording Experience

After solving any non-trivial problem (or giving up on one), record the experience:

```
record_experience(
  key="mem:episodic:...",
  effort_score=3,           # see guide below
  outcome="succeeded",      # succeeded | pivoted | abandoned
  iterations=2,
  abandoned_approaches=[
    {"name": "onnxruntime", "type": "library", "reason": "SIGILL on Alpine musl libc"}
  ],
  breakthrough="sentence-transformers with --prefer-binary pip flag",
  gotchas=["Needs openblas-dev and g++ installed in Alpine first"]
)
```

**Bug fixes must always be recorded.** After fixing any bug, call `remember()` with a structured description:

- **Symptom**: What the user saw (error message, HTTP status, unexpected behaviour)
- **Cause**: What was actually wrong
- **Fix**: What was changed to resolve it

For dead ends discovered mid-session, use `log_abandoned(key, name, type, reason)` to record them as they happen — do not wait until session end.

If `effort_score >= 4` and `outcome == "abandoned"`, the system will automatically suppress the abandoned approach names.

**Effort score guide:**

|Score|Meaning                                      |
|-----|---------------------------------------------|
|1    |Worked first time, no issues                 |
|2    |Minor friction, quick fix                    |
|3    |Multiple iterations, some debugging          |
|4    |Significant effort, approach changes required|
|5    |Near-abandonment, fundamental rethink        |

-----

### Session End

At the end of every session, without exception:

1. Call `update_project_state("<project_name>", current_state="<current state>", notes="<anything important for next session>")` — this is mandatory regardless of how much work was done
1. Call `remember()` for any key outcomes, decisions, or discoveries not already stored during the session
1. Ensure `record_experience()` has been called for all non-trivial work this session
1. If any contradictions were surfaced during the session but not resolved, note them in the project state update so the next session picks them up
1. If significant work was done this session (multiple memories stored, stack or goals changed), call `compile_project_context("<project_name>", auto_save=True)` to refresh the project context from the full set of memories. This keeps the context current without manual curation

-----

### Key Principles

- **Prefer `deprioritise` over `forget`** — humans usually mean "stop surfacing this" not "destroy this forever"
- **Always include a `reason` when deprioritising** — it helps future sessions understand the context
- **Include `reinstate_hints` when relevant** — if a memory might matter again under different circumstances, say so
- **Check the graveyard before agreeing to anything** — the list of what failed is as valuable as the list of what worked
- **Tag consistently** — inconsistent tags degrade recall quality over time; use the vocabulary above
- **Contradiction warnings are blockers** — do not proceed past session start with unresolved contradictions without at least surfacing them to the human
- **Never store secrets, credentials, or personally sensitive data in memory**
"""
