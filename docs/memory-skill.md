# Skill Memory Specification (v6)

**Key format**: `mem:skill:gen:{domain}-{user}`
**Created by**: `compile_skill()` only — propose-and-accept gated, shared with the web UI via `memory/skill_compiler.py`
**Index**: `idx:skill`

A compiled skill is a SKILL.md document distilled from a domain's episodic experience, graveyard entries, and promoted knowledge. The raw memories are the source of truth; the skill is build output, like a binary. A memory error is noise (ranked and diluted by recall); a skill error is policy (the agent obeys it) — which is why nothing writes to this namespace silently.

## Identity

- **Domain**: normalised to lowercase kebab-case, 1-64 chars of `[a-z0-9._-]`. Aliases resolve first (`py` → `python`, `k8s` → `kubernetes`, etc. — see `DOMAIN_ALIASES` in `memory/skills.py`), and an embedding "did you mean" guard (`SKILL_DOMAIN_SUGGEST_THRESHOLD`, default 0.60) catches near-misses so lessons don't scatter across synonym domains.
- **User**: from `OMNIMEM_USER` (default `local`), normalised the same way. Single-node in v6; org scoping is v7 territory.
- The `gen:` segment namespaces compiler output so a generated skill and a hand-authored one can never collide by construction.

## Fields

| Field | Format | Required | Description |
|-------|--------|----------|-------------|
| `name` | string | yes | `{domain}-{user}`, e.g. `python-local`. |
| `description` | string | yes | The auto-load trigger cue. Human-owned and pinned: the compiler drafts it for a brand-new skill only; recompiles keep the stored text unless an explicit override is passed. |
| `domain` | string | yes | Canonical domain. |
| `user` | string | yes | Identity segment. |
| `body` | string, max 100,000 chars | yes | The full rendered SKILL.md (structure below). Whole-document by design: deliberately **absent** from the search return whitelist, never chunked, fetched intact by ID via `get_skill()`. |
| `generated` | `"true"` | yes | The compiler refuses to overwrite any record not flagged `generated: true`. |
| `state` | lifecycle state | yes | `active` on write. |
| `surface_score` | float string | yes | `"1.0"`. |
| `contract_version` | int string | yes | Version of the fixed operating-contract block (currently 1). Bumping the contract text makes the next recompile propose the new block as a normal diff. |
| `compiled_at` | unix seconds string | yes | When the accepted proposal was compiled (carried from the proposal, not the write time). |
| `created_at` | unix seconds string | yes | First commit; preserved across recompiles. |
| `updated_at` | unix seconds string | yes | Last commit. |
| `tags` | JSON array | yes | `[domain]`. |
| `source_manifest` | JSON array of key strings | yes | Every memory key cited by any rule, sorted. |
| `rule_manifest` | JSON array | yes | The compiled rules as data, used by recompile diffs (`summarise_rule_changes`). Entry shape below. |
| `recall_count` / `last_recalled` | int string / unix seconds string | no | Bumped by `get_skill()`; feeds telemetry, which substitutes name + description for the missing `content`. |
| `vector` | 384-dim float32 blob | yes | Embeds **discovery metadata only**: `"{name}. {description} Domain: {domain}."` — never the body. `find_skills()` and briefing suggestions run relevance over the description because it is the load trigger. |

### `rule_manifest` entry shape

```json
{
  "kind": "do" | "watch" | "dont" | "ref",
  "text": "rule wording (newest source's phrasing wins)",
  "sources": ["mem:episodic:...", "..."],
  "reinforcement": 2,
  "blessed": true,          // only when a blessed memory carried it past the gate
  "name": "approach name",  // dont: the graveyard identity; ref: the article title
  "url": "https://..."      // ref only: the article's source URL
}
```

## Body structure

`render_skill_md()` produces, in order: YAML frontmatter (name, JSON-quoted description, `generated: true`, `source: omnimem`, domain, compiled_at as ISO 8601, contract_version, and a `source_manifest` list with per-key annotations like `# reinforced x3`, `# graveyard: <name>`, `# blessed`, `# promoted reference`), the generated banner, the fixed operating contract, then `## Do`, `## Watch out`, `## Don't (and why)`, `## Reference (promoted knowledge)`, and `## Provenance`. Every rule bullet cites its primary source key.

Rendering is deterministic: the same source memories produce a byte-identical body except the `compiled_at` frontmatter line (`bodies_equivalent()` strips exactly that line). Don't introduce randomness, dict-order dependence, or extra timestamps, or every recompile will propose noise diffs.

## The write gate

Experience and graveyard writes flow freely; the gate sits only at compile-to-skill:

1. **`compile_skill(mode="propose")`** gathers the domain pool, extracts and clusters lessons, applies the reinforcement gate (`min_reinforcement`, default 2, clamped 1-10), appends promoted reference rules, renders the body, and stashes the draft in `meta:skill:proposal:{domain}-{user}` with TTL `SKILL_PROPOSAL_TTL_SECONDS` (default 86,400). The response carries the full draft (new skill) or a unified diff plus a risk-classified change list (recompile: added/reinforced are low risk, rewritten/removed are high).
2. **`compile_skill(mode="write")`** commits the stashed body verbatim — no recompile at write time. It refuses when there is no live proposal, when the stored skill's body SHA changed since the proposal (`stale_proposal`), or when the existing record is not flagged `generated: true`. On success the proposal key is deleted. An optional `export_path` mirrors the body to a `.md` file under `SKILL_EXPORT_DIR` (path-traversal guarded).

### Proposal stash fields (`meta:skill:proposal:{domain}-{user}`)

| Field | Description |
|-------|-------------|
| `body` | The rendered draft, committed verbatim on write. |
| `description` | Resolved description (override > stored > compiler draft). |
| `domain`, `user` | Identity. |
| `based_on` | SHA-256 of the stored body at propose time (`""` for a new skill) — the staleness check. |
| `created_at` | Propose time; becomes the skill's `compiled_at`. |
| `min_reinforcement` | Gate setting used. |
| `rule_manifest`, `source_manifest` | JSON, copied onto the skill at commit. |

## Rule inputs and gates, summarised

| Input | Becomes | Gate |
|-------|---------|------|
| `breakthrough` on a succeeded episodic memory | Do rule | clusters at cosine ≥ `SKILL_CLUSTER_THRESHOLD` (0.80); needs `min_reinforcement` distinct source memories |
| `gotchas` | Watch out rule | same clustering and reinforcement gate |
| `abandoned_approaches` entries | Don't rule | grouped by approach name; same reinforcement gate |
| `bless()`-ed memory | any of the above (or its bare content as a Do) | bypasses the reinforcement threshold |
| `promote_knowledge(key, domain=...)` article | Reference rule(s) | promotion is the vetting; bypasses and never counts toward reinforcement |

## Lifecycle notes

- `skill` is a valid **search** namespace but not a valid `remember()` namespace.
- The web UI's `/skills` pages allow create and delete, never edit. Creation runs the same `compile_skill_flow`; recompiles stay on the MCP flow, which carries the diff review. Deleting a skill leaves its source memories intact, so recompiling the domain can recreate it.
- The briefing surfaces pending skill updates (source pools that changed since `compiled_at`) and `knowledge_watch()` matches (recent unpromoted articles semantically close to a skill's discovery vector, upgraded to `possible_contradiction` by the tier-1 negation heuristic).
