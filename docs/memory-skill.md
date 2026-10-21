# Skill Memory Specification

**Key format**: `mem:skill:gen:{domain}-{user}`
**Created by**: `compile_skill()` only, behind the propose-and-accept gate. The settings panel's New Skill dialog runs the same gate.

A compiled skill is a SKILL.md document distilled from a domain's experience, graveyard entries, promoted knowledge and influencing feeds. The raw memories are the source of truth and the skill is build output, like a binary. It matters because the two fail differently: a bad memory is noise (recall ranks it and dilutes it), but a bad skill is policy (the agent does what it says). That's why nothing writes to this namespace without you seeing it first.

Field formats follow the [storage model](memory-types.md#storage-model).

## Identity

- **Domain**: lowercased, whitespace turned into hyphens, then 1-64 characters of `[a-z0-9._-]`. Aliases resolve first (`py` → `python`, `k8s` → `kubernetes` and so on), and a "did you mean" guard based on embeddings (`SKILL_DOMAIN_SUGGEST_THRESHOLD`, default 0.60) catches near-misses so lessons don't scatter across synonyms.
- **User**: `OMNIMEM_USER` (default `local`), normalised the same way. A value that isn't valid falls back to `local`.
- The `gen:` segment keeps compiler output apart, so a generated skill and a hand-written one can never collide.

## Fields

| Field | Format | Description |
|-------|--------|-------------|
| `name` | string | `{domain}-{user}`, e.g. `python-local`. |
| `description` | string | The cue that makes an agent load the skill. It's yours: the compiler drafts one for a brand-new skill, and recompiles keep the stored text unless you pass a new one. |
| `domain` | string | Canonical domain. |
| `user` | string | The user segment. |
| `body` | string, max 100,000 characters | The whole rendered SKILL.md. Never chunked, never returned by search, fetched intact with `get_skill()`. |
| `generated` | `"true"` | The compiler refuses to overwrite a record without it. |
| `state` | lifecycle state | `active` when written. |
| `surface_score` | float string | `"1.0"`. |
| `contract_version` | int string | Version of the fixed operating contract (currently 1). Changing the contract text makes the next recompile propose the new block as an ordinary diff. |
| `compiled_at` | unix seconds string | When the accepted proposal was compiled (taken from the proposal, not the write). |
| `created_at` | unix seconds string | The first commit; kept across recompiles. |
| `updated_at` | unix seconds string | The latest commit. |
| `tags` | JSON array | `[domain]`. |
| `source_manifest` | JSON array of keys | Every memory key any rule cites, sorted. |
| `rule_manifest` | JSON array | The compiled rules as data, which recompile diffs compare against. Shape below. |
| `recall_count` / `last_recalled` | int string / unix seconds string | Bumped by `get_skill()`. Telemetry shows the name and description in place of `content`. |

The vector embeds **discovery metadata only**, `"{name}. {description} Domain: {domain}."`, never the body. `find_skills()` and the briefing's suggestions match on the description because that's what triggers a load. The store stamps the [v7 identity fields](memory-types.md#v7-identity-fields) on a skill too, apart from `content_hash`, since a skill has no `content`.

### `rule_manifest` entry shape

```json
{
  "kind": "do | watch | dont | ref | feed",
  "text": "the rule's wording (the newest source's phrasing wins)",
  "sources": ["mem:episodic:...", "..."],
  "reinforcement": 2,
  "blessed": true,
  "name": "approach name, article title or feed",
  "url": "https://..."
}
```

`blessed` only appears when a blessed memory carried the rule past the gate; `name` and `url` only when the rule has them (a don't rule's approach name, a reference's title and link).

## Body structure

The body is, in order:

1. YAML frontmatter: name, the description as a JSON-quoted string, `generated: true`, `source: omnimem`, domain, `compiled_at` as ISO 8601, `contract_version`, and a `source_manifest` list with notes on each key (`# reinforced x3`, `# graveyard: <name>`, `# blessed`, `# promoted reference`)
2. the generated-file banner
3. `## Operating contract` (fixed, the same in every skill)
4. `## How I work` and the compiled sections: `## Do`, `## Watch out`, `## Don't (and why)`, `## Reference  (promoted knowledge)`, `## Feed watch  (influenced feeds)`
5. `## Provenance`

Every rule bullet cites its main source key.

Rendering is deterministic: the same source memories give a byte-identical body apart from the `compiled_at` line. That's what keeps recompiles quiet when nothing has changed, and it's checked against bodies the 6.x Python rendered for the same inputs, so a skill imported from 6.x recompiles without a spurious diff.

## The write gate

Experience and graveyard writes flow freely. The gate only sits between compiling and committing:

1. **`compile_skill(mode="propose")`** gathers the domain's pool, pulls out and clusters the lessons, applies the reinforcement gate (`min_reinforcement`, default 2, held between 1 and 10), adds promoted reference rules and feed watch rules, renders the body and stashes the draft in `meta:skill:proposal:{domain}-{user}`, which expires after `SKILL_PROPOSAL_TTL_SECONDS` (default 86,400). You get the full draft back for a new skill, or a unified diff and a list of changes rated by risk for a recompile (added and reinforced rules are low risk; rewritten and removed ones are high).
2. **`compile_skill(mode="write")`** commits the stashed body exactly as proposed, with no recompile. It refuses when there's no live proposal, when the stored skill changed after the proposal was made (`stale_proposal`), or when the stored record isn't flagged `generated: true`. A successful commit deletes the proposal. `export_path` also writes the body to a `.md` file under `SKILL_EXPORT_DIR` (default `backups/skills` beside the database); the path has to be relative and can't climb out of that folder.

### Proposal fields (`meta:skill:proposal:{domain}-{user}`)

| Field | Description |
|-------|-------------|
| `body` | The rendered draft, committed as it is. |
| `description` | The description to commit: the one you passed, else the stored one, else the compiler's draft. |
| `domain`, `user` | Identity. |
| `based_on` | SHA of the stored body at propose time (`""` for a new skill): the staleness check. |
| `created_at` | Propose time; becomes the skill's `compiled_at`. |
| `min_reinforcement` | The gate setting used. |
| `rule_manifest`, `source_manifest` | JSON, copied onto the skill at commit. |

The briefing's automatic skill scan (every `SKILL_SCAN_INTERVAL_HOURS`, default 24) only ever creates proposals like these. It never commits one.

## Calling the tools

```python
# Step 1: propose.
compile_skill(
    domain="python",                # required; aliases resolve, near-misses get a suggestion
    mode="propose",                 # default
    min_reinforcement=2,            # default; held between 1 and 10
    include_graveyard=True,         # default; False leaves the Don't section's inputs out
    description="Python lessons learned on OmniMem",  # default None
)

# Step 2: review the draft or diff with the human, then commit it.
compile_skill(domain="python", mode="write", export_path="python.md")  # export_path optional

# Find skills by relevance: matches name, description and domain, never the body.
find_skills(query_or_domain="asyncio event loops")

# The whole SKILL.md. Accepts the key, the name ('python-local') or a bare domain.
get_skill(skill_id="mem:skill:gen:python-local")

# Feed the compiler: one strong episodic lesson...
bless(memory_key="mem:episodic:01KQ...")

# ...or a vetted article for the Reference section.
promote_knowledge(key="mem:knowledge:a1b2c3d4e5f60718", domain="python")
```

## Rule inputs and gates, summarised

| Input | Becomes | Gate |
|-------|---------|------|
| `lesson` (or `breakthrough`) on a succeeded episodic memory | Do rule | clusters at similarity `SKILL_CLUSTER_THRESHOLD` (0.80) or above; needs `min_reinforcement` distinct source memories |
| `gotchas` | Watch out rule | same clustering and reinforcement gate |
| `abandoned_approaches` entries | Don't rule | grouped by approach name; same reinforcement gate |
| a `bless()`-ed memory | any of the above, or its bare content as a Do | skips the reinforcement gate |
| `promote_knowledge(key, domain=...)` article | Reference rule(s) | promotion is the vetting; skips the gate, never counts towards it |
| a feed with a `skills:` score for the domain | Feed watch rules from its latest articles | skips the gate, capped by `SKILL_FEED_MAX_ARTICLES` (default 25); never creates a skill on its own |

## Lifecycle notes

- `skill` is a valid **search** namespace but not a valid `remember()` namespace.
- The settings panel's Skills pages let you create, delete, export and import skills, never edit one. Creating runs the same gate; recompiles stay with `compile_skill()`, which gives you the diff review. Deleting a skill leaves its source memories alone, so recompiling the domain can bring it back.
- Export bundles a skill with its source memories and influencing feeds into a checksummed zip; import is strictly additive and re-embeds everything locally. See [the skill compiler](skill-compiler.md).
- The briefing flags skills whose sources changed since `compiled_at`, and runs a knowledge watch: recent unpromoted articles close to a skill's description, upgraded to `possible_contradiction` when they look like they disagree with a rule.
