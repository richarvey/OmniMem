# The Skill Compiler

Memories tell an agent what happened. A skill tells it how you work.

A skill in OmniMem is not a set list of instructions, and it is never finished. Skills are built the way you actually build a skill: by doing the work, failing at some of it, succeeding at the rest, and noticing which lessons keep coming back. OmniMem compiles them from exactly that record, the work you did and the outcomes it produced. And they keep developing. As new experience and new dead ends land in memory, the skill evolves over time with your input and approval, so the agent gets faster and more precise at helping you reach your outcomes and goals.

`compile_skill("python")` distils your accumulated experience in a domain — reinforced breakthroughs, recurring gotchas, the graveyard of dead ends — into a loadable `SKILL.md`: do this, watch out for that, never try X again because it cost you an afternoon on that other project. Each rule cites the memories it came from. Load it at the start of Python work (or Rust work, or blog writing) and the agent works your way from the first prompt, which matters most on greenfield projects where no project context exists yet.

## The write gate

The design premise: a memory error is noise, ranked and diluted by recall, but a skill error is policy — the agent obeys it. So bad lessons cannot become policy silently:

- **A pattern earns a rule, an episode doesn't.** Lessons must recur across `min_reinforcement` memories (default 2) before they compile. One strong lesson can jump the queue if you `bless()` it.
- **Nothing writes silently.** `compile_skill` proposes a diff with a risk-classified change summary; you review it, then `mode="write"` commits exactly what you accepted. Recompiles that rewrite or remove an existing rule are flagged loudly; simple additions stay cheap.
- **Derived-only, never hand-edited.** The raw memories are the source of truth and the skill is build output. To change the guidance, update the memories and recompile.
- **Suggested, never auto-loaded.** The briefing recommends relevant skills; you and the agent decide.
- **Reference material is promoted, never absorbed.** Knowledge articles only reach a skill through `promote_knowledge(key, domain=...)` — a deliberate act that substitutes for the reinforcement an article can't earn. Promoted articles compile into a distinct Reference section, each citing its source. An article with discrete guidance (a "5 things to avoid" list) can be promoted with `rules=[{kind, text}, ...]` — the agent reads it, drafts the items, you approve them, and each becomes its own Avoid/Do/Watch bullet rather than one summary line. Extraction happens at promotion under review, never at compile, so compilation stays deterministic. Volatile facts (version numbers, latest releases) should stay in the knowledge namespace and be looked up with `recall()` instead.

## The flywheel

Every skill carries a fixed operating contract that instructs the agent to keep recording experience and dead ends while working under it. That closes the flywheel: the data pool compiles into a skill, the skill keeps feeding the pool, and a richer pool compiles a better skill next time.

The RSS feed acts as an early-warning system for the skills you've compiled: the briefing's knowledge watch compares recent articles against each skill and surfaces the ones that look relevant, flagging a possible contradiction when an article's language opposes one of the skill's rules. Nothing changes automatically — you review, promote the article into the skill if it belongs there, or ignore it and let it age out of the watch window.

## Storage

Skills live whole in Valkey (`mem:skill:gen:{domain}-{user}`) — discovery metadata is embedded and searchable, the body is retrieved intact, and `export_path` can mirror a copy to disk. Domains are free-form tags with a "did you mean" guard, so `py` resolves to `python` instead of silently scattering your lessons across tags that never reach the threshold.

The full field-by-field storage spec is in [memory-skill.md](memory-skill.md).

## Tools

| Tool | What it does |
|---|---|
| `compile_skill(domain, mode?, min_reinforcement?, include_graveyard?, export_path?, description?)` | Compile a domain's experience and graveyard into a `SKILL.md`. `propose` (default) returns a reviewable diff and change summary; `write` commits only a previously proposed and accepted draft — no silent writes. `export_path` mirrors the file under `SKILL_EXPORT_DIR` |
| `find_skills(query_or_domain)` | Ranked skill discovery over indexed metadata. Exact domain matches lead; a hand-authored skill outranks a generated one on the same domain |
| `get_skill(skill_id)` | Load the whole skill body intact, by key (`mem:skill:gen:python-ric`), name (`python-ric`), or bare domain (`python`) |
| `bless(memory_key)` | Promote one strong lesson to skill-eligible now, bypassing the reinforcement threshold at the next compile |
| `promote_knowledge(key, domain?, demote?, rules?)` | Feed a knowledge article into a domain's compiled skill as reference material (see [rss-knowledge.md](rss-knowledge.md)) |

## Tuning

The relevant environment variables, all covered in the [configuration reference](configuration.md): `OMNIMEM_USER`, `SKILL_CLUSTER_THRESHOLD`, `SKILL_DOMAIN_SUGGEST_THRESHOLD`, `SKILL_PROPOSAL_TTL_SECONDS`, `SKILL_SUGGEST_MIN_SIMILARITY`, `SKILL_EXPORT_DIR`, `SKILL_KNOWLEDGE_WATCH_DAYS`, `SKILL_KNOWLEDGE_WATCH_THRESHOLD`.
