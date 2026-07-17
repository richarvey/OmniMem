# Skill Demo Playbook: the `moo-reports` skill

A self-contained script for loading demo data into OmniMem, compiling a skill from it, showing the skill doing real work, and wiping every trace afterwards. The domain is deliberately fictional (writing SquareCows "weekly moo reports") so nothing collides with real memories, and everything lives under one tag and one project name for easy cleanup.

The demo data is fictional and the thresholds cited are verified against a live server, so this doubles as a worked example of the skill compiler's gates.

## What the demo shows

1. Experience memories, graveyard entries, a blessing and a promoted knowledge article flowing into one domain pool
2. `compile_skill` clustering those lessons into rules, with the reinforcement gate visibly excluding a one-off lesson
3. The propose-and-accept write gate (review the draft, then commit it)
4. A fresh session using the compiled skill to produce a correctly-formatted moo report — the demoable outcome

## How the compiler decides (the numbers behind the demo)

- **Domain membership is a tag.** Any active episodic memory tagged `moo-reports` (case-insensitive) joins the pool.
- **Lessons come from experience fields**: a `breakthrough` on a succeeded memory becomes a **do** rule, `gotchas` become **watch** rules, graveyard entries become **don't** rules. A blessed memory with no experience fields contributes its content as a do rule.
- **The gate**: a rule needs **2 distinct source memories** (clustered at cosine ≥ 0.80, so near-paraphrases count as the same lesson) or a **bless** to make it into the skill. This is why every rule below is seeded twice, except the blessed one.
- **Promoted knowledge bypasses the gate** — promotion is the human vetting step. It renders as `ref` rules in a Reference section.
- Keep the memory *contents* clearly different episodes (dedup fires at 0.92 on content) while keeping the *lesson texts* near-paraphrases (clustering fires at 0.80). The lesson texts below are verified against all-MiniLM-L6-v2: A/B's original gotcha phrasings scored *below* 0.80 and failed to cluster, so B's gotcha was rewritten as a closer paraphrase of A's. If you edit the lesson texts, re-check they still cluster (a dry-run `compile_skill(mode="propose")` shows anything held back at reinforcement 1).
- All outcomes below are `succeeded` with effort ≤ 3, so the auto-suppression path (abandoned + effort ≥ 4) never fires and cleanup stays simple.

## Step 1 — Load the episodic memories

Capture the `key` from each `remember()` response — you need it for `record_experience`, `bless`, and cleanup. Keep a scratch note like:

| Memory | Key |
|--------|-----|
| A (episode 1) | `mem:episodic:...` |
| B (episode 2) | `mem:episodic:...` |
| C (blessed) | `mem:episodic:...` |
| D (gate-excluded) | `mem:episodic:...` |
| E (promoted knowledge) | `mem:knowledge:...` |
| F (optional watch article) | `mem:knowledge:...` |

**Memory A** — first report episode:

```
remember(
  content="Wrote the first SquareCows weekly moo report. Early drafts opened with narrative about the week; stakeholders skimmed straight past. Restructured to open with the headline milk yield figure and engagement jumped.",
  project="skill-demo",
  tags=["moo-reports", "demo"]
)

record_experience(
  key="<key A>",
  effort_score=3,
  outcome="succeeded",
  iterations=3,
  breakthrough="Lead the moo report with the headline total milk yield figure before any narrative.",
  gotchas="Pasture sensor totals lag by a day, so label every yield figure with the sensor date rather than the report date.",
  abandoned_approaches=[{
    "name": "freeform-prose-report",
    "type": "approach",
    "reason": "Unstructured prose buried the yield numbers; nobody read past the first paragraph."
  }]
)
```

**Memory B** — second episode, reinforcing the same lessons in different words:

```
remember(
  content="Second moo report cycle. Confirmed last week's structure works: yield number up top, dated figures, structured sections. Tried reverting to a looser narrative style for variety and it flopped again.",
  project="skill-demo",
  tags=["moo-reports", "demo"]
)

record_experience(
  key="<key B>",
  effort_score=2,
  outcome="succeeded",
  iterations=2,
  breakthrough="Putting the headline total milk yield at the very top of the moo report is what stakeholders actually respond to.",
  gotchas="Pasture sensor yield totals lag by a day, so always label yield figures with the sensor date rather than the report date.",
  abandoned_approaches=[{
    "name": "freeform-prose-report",
    "type": "approach",
    "reason": "Loose narrative prose hides the yield figures and readers give up after one paragraph."
  }]
)
```

**Memory C** — a single-source rule that survives via blessing:

```
remember(
  content="Every moo report ends with a 'cow of the week' highlight. Keeps the tone human and gives the team something to look forward to.",
  project="skill-demo",
  tags=["moo-reports", "demo"]
)

bless(key="<key C>")
```

**Memory D** — the gate victim (one source, no bless — it should be *excluded*, and the compile output will say so):

```
remember(
  content="Experimented with adding a five-day weather forecast section to the moo report. Looked clever but nobody used it.",
  project="skill-demo",
  tags=["moo-reports", "demo"]
)

record_experience(
  key="<key D>",
  effort_score=1,
  outcome="succeeded",
  iterations=1,
  breakthrough="A weather forecast section makes the moo report feel more complete."
)
```

## Step 2 — Load and promote the knowledge article

**Memory E** — a style-guide article, promoted into the domain with an extracted rule:

```
remember(
  content="SquareCows moo report style guide: reports are written in British English with a conversational tone. Figures use litres, dates are day-month, and jargon is banned. The report should read like a farmer talking, not a spreadsheet.",
  namespace="knowledge",
  project="skill-demo",
  tags=["moo-reports", "demo"]
)

promote_knowledge(
  key="<key E>",
  domain="moo-reports",
  rules=[{"kind": "do", "text": "Write moo reports in British English with a conversational, farmer-to-farmer tone."}]
)
```

**Memory F (optional flourish)** — leave this one *unpromoted*. After the skill compiles, `briefing()` knowledge watch will flag it against the skill, and the negation heuristic should upgrade it to `possible_contradiction`:

```
remember(
  content="Draft moo-reports policy: moo reports should never end with individual cow highlights. Close every moo report with herd-level statistics only.",
  namespace="knowledge",
  project="skill-demo",
  tags=["moo-reports", "demo"]
)
```

Two verified-against-the-live-server caveats on this beat:

- The wording matters. Knowledge watch compares the article against the skill's *discovery* embedding (name + description + domain) with a 0.35 floor. The wording above scores 0.396; an earlier draft phrased as "Draft policy note: ... keep the closing section strictly to herd-level statistics" scored 0.341 and never surfaced. Lead with "moo-reports"/"moo reports" and keep the sentences short.
- The tier-1 negation heuristic may attribute the conflict to the *wrong rule* (in a live run it cited the sensor-date watch rule, not the cow-of-the-week rule). The `possible_contradiction: true` flag still lands, so present it as "the watch spotted policy drift", not "it knows which rule is threatened".

## Step 3 — Compile the skill

Propose first, review, then write:

```
compile_skill(domain="moo-reports", mode="propose")
```

Things worth showing in the draft:

- Two **do** rules: the headline-yield rule (reinforced x2) and the blessed cow-of-the-week rule (marked blessed)
- One **watch** rule: sensor-date labelling (reinforced x2)
- One **don't** rule: `freeform-prose-report` from the graveyard (reinforced x2)
- One **ref** rule in the Reference section: British English, from the promoted article
- The weather-forecast lesson listed as **excluded** — one source, below the 2-reinforcement gate. This is the "OmniMem doesn't let one-off opinions become policy" beat.

Then commit the exact draft you just reviewed (no recompile happens at write time):

```
compile_skill(domain="moo-reports", mode="write")
```

Verify:

```
find_skills("how do I write a moo report")
get_skill("mem:skill:gen:moo-reports-<user>")
```

The skill also appears at `/skills` in the web UI, with each rule linking back to its source memory — good for the "provenance" beat.

## Step 4 — The demoable outcome

In a fresh session (or after a `briefing(project="skill-demo")`, which should surface the skill as relevant), give Claude this prompt:

> Check OmniMem for anything about moo reports, then write this week's SquareCows moo report from this data: total yield 4,210 litres (sensor date Monday 13 July), top producer Buttercup at 61 litres/day, two calves born, fence repair completed in the north paddock.

The output visibly obeys the compiled rules, and you can tick them off live:

1. Headline yield figure first (do, reinforced x2)
2. Figure labelled with the *sensor* date, not today (watch)
3. Structured sections, not freeform prose (don't, from the graveyard)
4. Ends with a cow of the week (blessed rule — Buttercup, obviously)
5. British English, conversational (ref, from promoted knowledge)

Optional second beat: run `briefing(project="skill-demo")` and show memory F flagged by knowledge watch as a possible contradiction with the compiled skill — the awareness layer catching policy drift before it silently lands.

## Step 5 — Cleanup

Order matters slightly: delete the skill first, then the memories.

1. **Delete the skill** in the web UI: `/skills` → moo-reports → Delete (with confirmation). There is deliberately no MCP delete for skills.
2. **Forget every memory** from your key table (A, B, C, D, E, and F if created):
   ```
   forget(key="<each key>")
   ```
   Graveyard entries live on the memories, so they go too.
3. **Remove the project**: `delete_project("skill-demo")` if you created a project context, otherwise skip — without `set_project_context` the project only ever existed as labels on the memories you just forgot.
4. Nothing else lingers: the compile proposal stash (`meta:skill:proposal:moo-reports-*`) expires on its own TTL (24h default), and no suppressions were created because no abandoned outcome had effort ≥ 4.

Quick verification that it's all gone: `find_skills("moo")` returns nothing, `recall("moo report")` returns nothing, and `/memories` filtered by the `demo` tag is empty.
