## OmniMem — Persistent Semantic Memory

You have access to a persistent memory system via the `omnimem` MCP server. It is the primary persistent memory store for all sessions.

-----

### Tool Priority

Before using web_search or answering from training data, ALWAYS query OmniMem first:

1. Call `recall("<relevant query>")` or `recall_index("<relevant query>")` to check for prior solutions, patterns, or knowledge articles
2. Only fall back to web_search if OmniMem returns nothing useful
3. Combine both when recency matters — use OmniMem for project context and prior decisions, web for the latest information

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
- **Skill suggestions** — if the briefing recommends compiled skills, offer them to the human; on a greenfield project (no context yet) lead with them. Load with `get_skill()` only if agreed — never auto-load
- **Skill updates** — pending changes to compiled skills. Low-risk additions can be accepted in a batch; rewrites or removals of existing rules must be reviewed individually via `compile_skill(domain, mode="propose")`
- **Knowledge watch** — if the briefing includes `skill_knowledge_watch`, recent articles look relevant to a compiled skill; entries flagged `possible_contradiction` may mean the world moved under a rule. Surface them and, if the human agrees an article belongs in the skill, call `promote_knowledge(key, domain="<domain>")` then recompile
- **Auto-proposed skills** — if the briefing includes `auto_proposed_skills`, the server's periodic scan found recurring cross-project lessons worth a new skill (or a changed skill worth a fresh draft) and has already stashed the proposal. Surface each one; review with `compile_skill(domain, mode="propose")` and accept with `mode="write"` only if the human agrees. Ignoring a draft declines it
1. If the MCP server seems unresponsive or recall is slow, call `health()` and report the status to the human before continuing.

-----

### During a Session

**Before attempting any problem** where you would normally reach for documentation or a search engine:

- Call `recall("<problem description>")` — you may find a prior solution, a relevant pattern, or a knowledge article that gives you a head start
- If a recalled knowledge article seems relevant, mention it: *“I found an article from [source] about X — shall I use that as a research base?”*

**Before suggesting OR agreeing to any library, tool, or architectural approach** — including ones the human proposes:

- Call `warn_if_abandoned("<library or approach name>")`
- If a warning comes back, tell the human before proceeding: *“We tried [X] before and abandoned it because [reason] — shall we try again or look for alternatives?”*
- Do not skip this check because the human suggested the approach. Dead ends are dead ends regardless of who proposed them.

**Store memories proactively.** When you learn something worth keeping, write it to OmniMem using `remember()`. Do not wait to be asked. Call `remember()` when you:

- Reach a decision or agree on an approach
- Solve a tricky or non-obvious bug
- Discover a pattern, constraint, or gotcha
- Complete a meaningful piece of work
- Learn a preference, a brand rule, or a technical choice

**Choose the right namespace:**

- `episodic` — things that happened: decisions made, work done, bugs fixed (default)
- `knowledge` — facts, rules, preferences, reference information
- `project` — scoped context for a specific project

Use this tagging vocabulary for consistency:

|Category|Example tags                                                        |
|--------|--------------------------------------------------------------------|
|Stack   |`rust`, `python`, `docker`, `traefik`, `valkey`, `n8n`              |
|Intent  |`bug-fix`, `decision`, `pattern`, `gotcha`, `config`, `architecture`|
|Outcome |`working`, `deprecated`, `revisit`                                  |
|Project |use the project name as a tag                                       |

Always include at least one stack tag and one intent tag.

**If an existing memory is missing tags or tagged wrongly**, fix it with `retag()` — `retag(key, add=[...])` and `retag(key, remove=[...])` adjust the existing set, `retag(key, tags=[...])` replaces it outright. Tags feed recall filtering and skill compiler domains, so tidying them is worthwhile.

**If the human says** something like “forget about X”, “stop bringing up Y”, or “I don’t do that anymore”:

- Call `deprioritise()` with a clear reason
- Add `reinstate_hints` if the memory might become relevant again in a different context
- Do NOT hard delete unless they say “permanently delete” or “wipe”

**If a recalled memory keeps surfacing when it clearly shouldn’t:**

- Call `suppress_topic("<topic>")` and let the human know it has been suppressed

**Automatic maintenance** — every 10 `briefing()` calls per project (configurable via `AUTO_MAINTENANCE_INTERVAL`), the server automatically:

- Scans for duplicate memories and archives the oldest in each cluster
- Runs a heuristic contradiction scan on active project memories

When maintenance runs, the briefing response includes an `auto_maintenance` section showing what was cleaned up. You can still call `find_duplicates()` and `check_contradictions()` manually at any time. Set `AUTO_MAINTENANCE_INTERVAL=0` to disable.

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

### Skills (compiled procedure)

OmniMem can compile your accumulated experience in a domain into a loadable skill via `compile_skill("<domain>")` — do/don't/watch-out rules distilled from reinforced breakthroughs, gotchas, and the graveyard, each citing its source memories. Domains are tags: a memory tagged `python` feeds the `python` skill.

**Loading.** When `briefing()` suggests a skill (or `find_skills("<query or domain>")` finds one), offer it to the human and load it with `get_skill("<skill_id>")` if they agree. Never load one silently. Once loaded, follow its operating contract: keep recording experience and dead ends while you work, so the next compile is better.

**Compiling and updating.** Skills are derived output — never edit one by hand; update the underlying memories and recompile. The flow is always propose, review, accept:

1. `compile_skill("<domain>")` returns a diff (or a full draft for a new skill) plus a risk-classified change summary
1. Show the human the changes — additions are low-stakes; rewrites or removals of existing rules deserve individual attention
1. Only after they accept, call `compile_skill("<domain>", mode="write")` — it commits exactly the proposed draft, nothing else

The skill's `description` is the load trigger and is human-owned: the compiler drafts it once at creation, the human approves or edits it (pass `description=` to change it deliberately), and recompiles never clobber it.

**Promotion.** A single episode is a memory; a pattern across episodes earns a rule (default `min_reinforcement=2`). When one lesson is strong enough on its own, call `bless("<memory_key>")` to make it skill-eligible at the next compile.

**Reference material.** Knowledge articles can feed a skill too, but only deliberately: `promote_knowledge(key, domain="<domain>")` marks an article skill-eligible, and the next compile renders it in a distinct Reference section citing the article. Promotion is the vetting step — never promote without the human agreeing the article belongs in the skill. Use it for durable reference (a spec, a canonical how-to), not volatile facts like version numbers; those stay in the knowledge namespace and are looked up with `recall()` at need. `demote=True` reverses a promotion.

**Extracting rules from an article.** When an article contains discrete guidance (a "5 things to avoid" list, a best-practice post), don't settle for the one-line summary: read the article (`recall_detail` on its key), draft one rule per item as `{"kind": "do"|"watch"|"dont"|"note", "text": "..."}`, show the human the list, and pass the approved set as `promote_knowledge(key, domain="<domain>", rules=[...])`. Each becomes its own stance-prefixed bullet in the skill's Reference section ("Avoid: ...", "Do: ..."), all citing the article. Extraction happens at promotion under human review — never at compile — so re-promote with an edited list to revise, or `rules=[]` to revert to the summary line.

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

- **Prefer `deprioritise` over `forget`** — humans usually mean “stop surfacing this” not “destroy this forever”
- **Always include a `reason` when deprioritising** — it helps future sessions understand the context
- **Include `reinstate_hints` when relevant** — if a memory might matter again under different circumstances, say so
- **Check the graveyard before agreeing to anything** — the list of what failed is as valuable as the list of what worked
- **Tag consistently** — inconsistent tags degrade recall quality over time; use the vocabulary above
- **Contradiction warnings are blockers** — do not proceed past session start with unresolved contradictions without at least surfacing them to the human
- **Never store secrets, credentials, or personally sensitive data in memory**