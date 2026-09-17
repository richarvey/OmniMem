## OmniMem — Persistent Semantic Memory

Persistent memory across sessions, via the `omnimem` MCP server. The full
guide is `docs/agent-guide.md`; this is the working summary.

### Before you answer, and before you agree

1. `recall("<query>")` before reaching for web search or answering from
   training data. Add `domain_filter="<kind of work>"` when the problem is
   about a technology rather than this project.
2. **Before suggesting *or agreeing to* any library, tool or approach** —
   including one the human proposes — call `warn_if_abandoned("<name>")`. If
   it warns, say so before going further: *"We tried X and abandoned it
   because Y — try again, or find another way?"* Dead ends are dead ends
   regardless of who proposed them, and re-deriving one costs far more than
   the check.
3. Fewer results than `top_k` is a real answer. Anything below the relevance
   floor is dropped rather than padding the list, so an empty return means
   nothing stored is relevant. Say so and move on.
4. A `weak_match` scored in the band where relevant and irrelevant results
   overlap. Read it, do not build on it, and do not construct a connection
   between it and your query.

### Session start

Call `briefing(project="<name>")`. It aggregates project context, experience,
stale memories, contradiction warnings and reinstate candidates in one call.
Summarise what came back, and surface any contradiction warnings explicitly
before doing work — they are blockers, not notes.

### Storing

Call `remember()` when you reach a decision, fix a non-obvious bug, discover a
constraint or gotcha, or learn a preference. Do not wait to be asked.

- Namespaces: `episodic` (what happened, the default), `knowledge` (facts and
  rules), `preference`, `project`.
- Tag with at least one stack tag (`rust`, `docker`, `python`) and one intent
  tag (`decision`, `gotcha`, `bug-fix`, `pattern`).
- Pass `licence=` when the content is someone else's (`restricted` for
  paywalled or vendor material, `open` or an identifier for redistributable),
  and `provenance="asserted"` when the human stated it rather than you
  concluding it. A later session must be able to tell evidence from your own
  inference.

### Recording experience, especially dead ends

After any non-trivial problem, solved or abandoned:

```
record_experience(
  key="mem:episodic:...",
  effort_score=4,                 # 1 first time, 3 several iterations, 5 near-abandonment
  outcome="abandoned",            # succeeded | pivoted | abandoned
  abandoned_approaches=[{"name": "...", "type": "library", "reason": "..."}],
  breakthrough="what finally worked",
  lesson="the claim that holds beyond this incident, if there is one"
)
```

At `effort_score >= 4` with `outcome="abandoned"`, the approach names are
suppressed automatically, so a later session is not sent back down the same
path. Record dead ends as they happen with
`log_abandoned(key, name, type, reason)` rather than waiting for session end.

Bug fixes are always worth a `remember()`: symptom, cause, fix.

### Skills

Compiled procedure for a domain. `briefing()` suggests them and
`find_skills("<query>")` finds them; load with `get_skill()` only after the
human agrees, never silently. Build and change them through
`compile_skill(domain, mode="propose")`, then `mode="write"` once they accept.
Skills are derived output: update the underlying memories, never edit a skill
by hand. See `docs/agent-guide.md` for promotion, blessing and feed influence.

### Session end

Call `update_project_state("<project>", current_state=..., notes=...)` every
time, without exception, and `remember()` anything not already stored.

### Principles

- Check the graveyard before agreeing to anything. What failed is as valuable
  as what worked.
- Prefer `deprioritise` (always with a `reason`) over `forget`. Humans usually
  mean "stop surfacing this", not "destroy it".
- Never store secrets, credentials, or personally sensitive data.
