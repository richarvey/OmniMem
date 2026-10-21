# The Skill Compiler

Memories tell an agent what happened. A skill tells it how you work.

A skill in OmniMem isn't a list of instructions somebody wrote once, and it's never finished. It's built the way you build a skill yourself: by doing the work, failing at some of it, getting the rest right, and noticing which lessons keep coming back. OmniMem compiles skills from exactly that record. As new experience and new dead ends land, the skill evolves with them (with your say-so), so the agent gets quicker and more precise at helping you get where you're going.

`compile_skill("python")` distils your accumulated experience in a domain (reinforced breakthroughs, recurring gotchas, the graveyard of dead ends) into a loadable `SKILL.md`: do this, watch out for that, never try X again because it cost you an afternoon on that other project. Every rule cites the memories it came from. Load it at the start of some Python work (or Rust, or blog writing) and the agent works your way from the first prompt. That matters most on a greenfield project, where there's no project context yet to lean on.

## The write gate

The premise is simple. A bad memory is noise: recall ranks it, dilutes it, and you move on. A bad skill is policy: the agent obeys it. So a bad lesson can't become policy quietly.

- **A pattern earns a rule, an episode doesn't.** A lesson has to recur across `min_reinforcement` memories (2 by default) before it compiles. One strong lesson can jump the queue if you `bless()` it.
- **Nothing writes silently.** `compile_skill` proposes a diff with a change summary sorted by risk. You review it, then `mode="write"` commits exactly what you accepted. Recompiles that rewrite or remove a rule are flagged loudly; simple additions stay cheap.
- **Derived, never hand-edited.** The memories are the source of truth and the skill is build output. Want different guidance? Update the memories and recompile.
- **Suggested, never auto-loaded.** The briefing recommends skills. You and the agent decide what loads.
- **Reference material is promoted, never absorbed.** Articles only reach a skill through `promote_knowledge(key, domain=...)`, a deliberate act that stands in for the reinforcement an article can't earn. Promoted articles compile into their own Reference section, each citing its source. An article with discrete guidance (a "5 things to avoid" list, say) can be promoted with `rules=[{kind, text}, ...]`: the agent drafts the items, you approve them, and each becomes its own bullet rather than one summary line. That extraction happens at promotion, under review, never at compile, so compiling stays deterministic. Volatile facts like version numbers belong in the knowledge namespace, looked up with `recall()`.

## The flywheel

Every skill carries a fixed operating contract telling the agent to keep recording experience and dead ends while it works under that skill. That closes the loop: the pool compiles into a skill, the skill keeps feeding the pool, and a richer pool compiles a better skill next time.

Your RSS feeds double as an early warning system. The briefing's knowledge watch compares recent articles (the last `SKILL_KNOWLEDGE_WATCH_DAYS`, 14) against each skill and surfaces the relevant ones, flagging a possible contradiction when an article's language opposes one of the skill's rules. Nothing changes by itself. You review it, promote the article if it belongs, or ignore it and let it age out.

## Feed influence

Promotion is vetting one article at a time. Feed influence is the standing version of the same decision. In `feeds.yml` (or the settings panel's feed editor) you can tie a feed to one or more skill domains with a score from 1 to 10. When that skill is recompiled, the latest articles from its influencing feeds are pulled into a **Feed watch** section automatically, and the score is literally how many of the feed's most recent articles appear. A 10 dominates the section, a 1 adds a single headline, and the whole section is capped at `SKILL_FEED_MAX_ARTICLES` (25, and 0 leaves it out).

The write gate still applies. Feed watch items only arrive through a proposed, reviewed compile, the section is labelled as unvetted current signal rather than procedure, and its natural churn (articles arrive, articles expire) counts as low risk in the change summary, so rotations don't drown out real rule changes. Feeds don't bootstrap a skill either: a domain with no experience and no promoted references still won't compile from a feed alone.

Exported skill bundles carry the influencing feeds (name, URL, topics, mode and score), so a skill stays fed wherever you import it. Feeds missing from the receiving reading list are added; feeds already there at most gain the influence entry they lacked. Nothing existing is rewritten.

## Storage

A skill is stored whole under `mem:skill:gen:{domain}-{user}`, where the user part is `OMNIMEM_USER` (`local` by default). Its discovery metadata (name, description, domain) is embedded for search; the body is kept intact and returned intact. `export_path` mirrors a copy to disk under `SKILL_EXPORT_DIR` (a `skills` folder in the backup folder by default).

Domains are free-form, with a "did you mean" guard, so `py` resolves to `python` instead of scattering your lessons across tags that never reach the threshold.

Compiling is deterministic: the same source memories render a byte-identical body apart from the `compiled_at` line. The Rust compiler is checked against a golden fixture the 6.7.1 Python wrote, so skills imported from 6.x recompile with no diff. (Until cut-over the banner in a compiled skill still says Valkey, because changing it would turn every recompile into a diff.)

The field-by-field spec is in [memory-skill.md](memory-skill.md).

## Tools

| Tool | What it does |
|---|---|
| `compile_skill(domain, mode?, min_reinforcement?, include_graveyard?, export_path?, description?)` | Compile a domain's experience and graveyard into a `SKILL.md`. `propose` (default) returns a reviewable diff; `write` commits only a previously proposed and accepted draft. The description is yours: set it with `description`, and recompiles keep it |
| `find_skills(query_or_domain)` | Ranked skills above `SKILL_MIN_SCORE` (0.25), each with a `high` or `low` confidence. An empty list is a real answer |
| `get_skill(skill_id)` | The whole body, by key (`mem:skill:gen:python-ric`), name (`python-ric`) or bare domain (`python`) |
| `bless(memory_key)` | Let one strong lesson past the reinforcement gate at the next compile |
| `promote_knowledge(key, domain?, demote?, rules?)` | Feed an article into a domain's skill as reference material (see [RSS and knowledge](rss-knowledge.md)) |

The settings panel's Skills page can also create a skill through the same propose-and-accept gate, delete one, and export or import bundles. See [the settings panel](settings-panel.md#skill-export-and-import).

## The auto skill scan

You don't have to notice that a skill is waiting to exist. At most once every `SKILL_SCAN_INTERVAL_HOURS` (24), a `briefing()` runs a scan that does two things:

- **New skills.** It looks for domains with no skill whose experience already carries rules that would clear the gate. By default (`SKILL_SCAN_CROSS_PROJECT`) a rule has to span two or more projects, because a lesson that recurs across projects is the strongest sign it deserves to be policy. A domain needs at least `SKILL_SCAN_MIN_POOL` (3) memories to be considered, and at most `SKILL_SCAN_MAX_PROPOSALS` (3) drafts are proposed per scan.
- **Changed skills.** Where a skill's sources have moved on, it compiles a fresh draft so the diff is ready to review.

Everything it produces is a proposal, exactly what `compile_skill(mode="propose")` makes, so the gate is untouched and a human still accepts every draft. Results show in the briefing's `auto_proposed_skills` section and as pending proposals on the settings panel's Skills page.

Ignoring a draft declines it. The proposal expires after `SKILL_PROPOSAL_TTL_SECONDS` (a day), and OmniMem remembers what it proposed so the identical draft isn't raised again. Only when the underlying lessons change does that domain come back round.

## Tuning

All of these are in the [configuration reference](configuration.md):

| Variable | Default | What it does |
|---|---|---|
| `OMNIMEM_USER` | `local` | The user part of generated skill names |
| `SKILL_CLUSTER_THRESHOLD` | 0.80 | Similarity that makes two lessons the same rule |
| `SKILL_DOMAIN_SUGGEST_THRESHOLD` | 0.60 | The "did you mean" guard for domains |
| `SKILL_PROPOSAL_TTL_SECONDS` | 86400 | How long a proposal stays committable |
| `SKILL_MIN_SCORE` | 0.25 | Relevance floor for `find_skills` |
| `SKILL_SUGGEST_MIN_SIMILARITY` | 0.30 | Floor for briefing skill suggestions |
| `SKILL_EXPORT_DIR` | `BACKUP_DIR/skills` | Where `export_path` mirrors write |
| `SKILL_KNOWLEDGE_WATCH_DAYS` | 14 | The knowledge watch window; 0 turns it off |
| `SKILL_KNOWLEDGE_WATCH_THRESHOLD` | 0.35 | How close an article has to be to a skill to be watched |
| `SKILL_FEED_MAX_ARTICLES` | 25 | Cap on the Feed watch section; 0 leaves it out |
| `SKILL_SCAN_INTERVAL_HOURS` | 24 | How often the auto scan runs; 0 turns it off |
| `SKILL_SCAN_CROSS_PROJECT` | on | Only propose rules that span projects |
| `SKILL_SCAN_MIN_POOL` | 3 | Smallest pool the scan considers |
| `SKILL_SCAN_MAX_PROPOSALS` | 3 | Most drafts per scan |
