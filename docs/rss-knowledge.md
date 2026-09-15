# RSS Feeds and the Knowledge Base

OmniMem's passive knowledge comes from RSS feeds you choose. They're fetched on a schedule, summarised by Claude Haiku, embedded and stored in the `knowledge` namespace. So when you're wrestling with a Rust problem and a relevant article landed last week, it turns up in recall as a starting point worth reading.

There's no separate worker any more. The RSS scheduler is a thread inside OmniMem itself, sharing the same engine and the same embedder.

## Configuring feeds

The reading list is `feeds.yml`, which lives beside the database (in the desktop app's data folder, `/var/lib/omnimem` for the Linux packages, `/data` in the Docker image). `FEEDS_CONFIG_PATH` points it somewhere else. The format hasn't changed since 6.x, so an old `feeds.yml` drops straight in:

```yaml
feeds:
  - url: https://blog.rust-lang.org/feed.xml
    name: Rust Official Blog
    topics: [rust, systems, language]

  - url: https://this-week-in-rust.org/rss.xml
    name: This Week in Rust
    topics: [rust, community, crates]
    mode: digest                   # one article per item in a roundup

  - url: https://blog.n8n.io/rss/
    name: n8n Blog
    topics: [automation, workflow, n8n]
    project: automation-research   # optional, defaults to "RSS"

  - url: https://realpython.com/atom.xml
    name: Real Python
    topics: [python]
    skills:                        # optional: influence compiled skills
      python: 8

  - url: https://www.gov.uk/government/publications.atom
    name: GOV.UK Publications
    topics: [regulation]
    licence: ogl-3.0               # optional: redistribution rights, see below
```

On the desktop you can do all of this from the [settings panel](settings-panel.md)'s RSS feeds page instead, including uploading or downloading the whole file.

## How ingestion works

A cycle runs when OmniMem starts, every `RSS_SCHEDULE_HOURS` (6) after that, and whenever `feeds.yml` changes (it's checked every `FEEDS_WATCH_INTERVAL` seconds, 10). Set `RSS_SCHEDULE_HOURS=0` and it only runs at start and on changes.

For each feed, up to `RSS_MAX_ARTICLES_PER_FEED` (20) new articles are fetched, stripped of HTML, summarised to a couple of sentences, embedded and stored with an expiry `MAX_KNOWLEDGE_AGE_DAYS` (30) after ingest. Articles are deduplicated by URL, and expired ones are archived during maintenance.

A feed in `digest` mode is a roundup: each item becomes its own article, up to `RSS_MAX_DIGEST_ENTRIES` (2) per cycle, and when an entry is only a teaser OmniMem fetches the page behind it. No page or feed read goes past `RSS_MAX_PAGE_BYTES` (10 MB).

Articles are labelled with the project `RSS` (or the feed's own `project:`) so they stay separate from knowledge you captured in conversation. Pass `project_filter="RSS"` to `recall()` to search only articles.

No `ANTHROPIC_API_KEY`? It still works. Summaries fall back to a truncated excerpt instead of Haiku.

A feed that can't be fetched is logged rather than silently treated as empty. To try a reading list without storing anything:

```bash
omnimem rss --dry-run   # fetch and parse, print what would be ingested
omnimem rss             # run one real cycle now
```

## Licence and redistribution rights

Every article gets a `licence` the moment it's stored: may this content be redistributed outside your instance? The answer comes from the feed's `licence:` declaration. It's set at ingest rather than audited later, because the scheduler piles up articles every night and picking out the ones you can't share after the fact is miserable. ("It's only a summary" isn't much of a defence once something's been sold.)

The value is one of four classes, and a recognised licence identifier resolves to a class and keeps the identifier as a note:

| Class | Meaning | Identifiers that resolve to it |
|---|---|---|
| `open` | Third-party content you may redistribute | `ogl-3.0`, `cc-by-4.0`, `cc-by-sa-4.0`, `cc0`, `public-domain`, `mit`, `apache-2.0` |
| `restricted` | Third-party content you may not redistribute | `all-rights-reserved`, `proprietary`, `paywalled`, `cc-by-nc`, `cc-by-nd`, `crown-copyright` |
| `own` | Written here: your own decisions, fixes and preferences | (the default for conversation memories) |
| `unknown` | Nobody has said yet | (the default for a feed that declares nothing) |

Non-commercial and no-derivatives Creative Commons licences count as restricted on purpose: a summary is a derivative, and a sold bundle is commercial use. An unrecognised identifier is logged and treated as `unknown` rather than guessed at.

A feed that declares nothing ingests as `unknown`, and `recall()` points those articles out in a `licence_notice` so you can classify them while they're in front of you: `set_licence(keys=[...], licence="open")`, or `set_licence(feed_name="...", licence="ogl-3.0")` for everything already stored from one feed. Then set `licence:` on the feed so future articles arrive classified. In the settings panel, the Memories page filters on `licence=unknown` and each memory's detail page has a form for it.

If your store must never pick up unvetted records, set `RSS_REQUIRE_LICENCE=true`. A feed with no usable licence is then skipped before anything is fetched, and counted as refused, instead of ingesting as `unknown`.

This field answers one question: redistribution rights. It says nothing about who may see a memory. A summary of a paywalled standard can be visible to everyone on your instance and still not be yours to share.

## Keeping articles

When an article turns out to be properly useful, `promote_knowledge(key)` clears its expiry and keeps it for good. `promote_knowledge(key, domain="python")` also feeds it into that domain's compiled skill as reference material. [The skill compiler](skill-compiler.md) explains how promoted articles become Reference rules.

## Influencing skills

Promotion vets one article at a time. When a whole feed reliably matters to a skill, give it a standing association instead: a `skills:` mapping ties the feed to one or more skill domains with a score from 1 to 10. Every recompile of that skill then pulls the feed's latest articles into a Feed watch section, and the score is how many of them the feed contributes, so a 10 dominates and a 1 adds a single headline. Feeds without a mapping never touch a skill unless you promote an article by hand. Details are in [the skill compiler](skill-compiler.md#feed-influence).

## See also

- [Knowledge memory spec](memory-knowledge.md): every stored field
- [Configuration reference](configuration.md): `RSS_SCHEDULE_HOURS`, `RSS_MAX_ARTICLES_PER_FEED`, `RSS_MAX_DIGEST_ENTRIES`, `RSS_MAX_PAGE_BYTES`, `RSS_REQUIRE_LICENCE`, `MAX_KNOWLEDGE_AGE_DAYS`, `FEEDS_CONFIG_PATH` and `FEEDS_WATCH_INTERVAL`
