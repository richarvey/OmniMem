# RSS Feeds and the Knowledge Base

OmniMem's passive knowledge comes from RSS feeds you configure. They get fetched on a schedule, summarised by Claude Haiku, embedded, and stored in the `knowledge` namespace. When you are working on a Rust problem and a relevant article was ingested last week, it surfaces as a starting point worth reading.

## Configuring feeds

Edit `rss_worker/feeds.yml` to choose which feeds get ingested:

```yaml
feeds:
  - url: https://blog.rust-lang.org/feed.xml
    name: Rust Official Blog
    topics: [rust, systems, language]

  - url: https://this-week-in-rust.org/rss.xml
    name: This Week in Rust
    topics: [rust, community, crates]

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

You can also manage feeds from the [web UI](web-ui.md)'s RSS Feeds page — uploading a new feeds.yml just writes the file and the worker picks up the change automatically.

## How ingestion works

Each article gets fetched, stripped of HTML, summarised to a couple of sentences by Claude Haiku, embedded, and stored in the `knowledge` namespace with an `expires_at` timestamp (default 30 days, configurable via `MAX_KNOWLEDGE_AGE_DAYS`). Articles are labelled with the project `RSS` (or the feed's own `project:` label if you set one) so ingested content stays separable from knowledge captured in conversation — filter by project in the web UI, or pass `project="RSS"` to `recall()` to search only articles. Expired articles are auto-archived during maintenance. Duplicates are skipped by URL. The worker runs once on startup and then on whatever schedule you set in `RSS_SCHEDULE_HOURS`.

If no `ANTHROPIC_API_KEY` is set, the worker still runs — summaries fall back to simple truncation instead of Haiku.

## Licence and redistribution rights

Every article is stamped with a `licence` field the moment it is stored: may this content be redistributed outside your instance? The answer comes from the feed's `licence:` declaration in feeds.yml (or the Licence field in the web UI's feed editor), and the field is set at ingest rather than audited later because the worker accrues articles every night and the ones you cannot ship are impossible to pick out cheaply afterwards. "It's only a summary" is a weak defence once a bundle has been sold.

The value is one of four classes, and a recognised licence identifier resolves to a class and keeps the identifier as a note:

| Class | Meaning | Identifiers that resolve to it |
|---|---|---|
| `open` | Third-party content you may redistribute | `ogl-3.0`, `cc-by-4.0`, `cc-by-sa-4.0`, `cc0`, `public-domain`, `mit`, `apache-2.0` |
| `restricted` | Third-party content you may not redistribute | `all-rights-reserved`, `proprietary`, `paywalled`, `cc-by-nc`, `cc-by-nd`, `crown-copyright` |
| `own` | Written here — your own decisions, fixes and preferences | (the default for conversation-sourced memories) |
| `unknown` | Nobody has said yet | (the default for a feed that declares nothing) |

Non-commercial and no-derivatives Creative Commons variants are restricted on purpose: a summary is a derivative, and a sold bundle is commercial use. An unrecognised identifier is logged and treated as `unknown` rather than guessed at.

A feed that declares nothing ingests as `unknown`, and `recall()` points those articles out with a `licence_notice` so the human can classify them when the content is in front of them — with `set_licence(keys=[...], licence="open")`, or `set_licence(feed_name="...", licence="ogl-3.0")` to classify every article already stored from one feed. Set `licence:` on the feed itself so future articles arrive classified. The web UI's memories page filters on licence (`?licence=unknown` is the classify queue) and each memory's detail page has a form for it.

If your store must never accrue unvetted records, set `RSS_REQUIRE_LICENCE=true`: a feed with no usable licence declaration is then refused outright — skipped before any fetch, counted under `refused` in the ingest stats — instead of ingesting as unknown. Articles you have already classified are never touched by the worker.

This field answers one question only: redistribution rights. It says nothing about who may see a memory. A summary of a paywalled standard can legitimately be visible to everyone on your instance and still be non-redistributable.

## Keeping articles

If an article turns out to be genuinely useful, call `promote_knowledge(key)` to clear its expiry and keep it permanently — or `promote_knowledge(key, domain="python")` to also feed it into that domain's compiled skill as reference material. See [the skill compiler](skill-compiler.md) for how promoted articles become Reference rules.

## Influencing skills

Promotion vets one article at a time. When a whole feed reliably matters to a skill, give it a standing association instead: a `skills:` mapping (shown above, also editable per feed in the web UI) ties the feed to one or more skill domains with an influence score from 1 to 10. Every recompile of that skill then pulls the feed's latest articles into a Feed watch section automatically — the score is how many recent articles the feed contributes, so 10 dominates and 1 adds a single headline. Feeds without a `skills:` mapping keep today's behaviour: their articles never touch a skill unless you promote one by hand. Details and the review-gate implications are in [the skill compiler](skill-compiler.md#feed-influence).

## See also

- [Knowledge memory spec](memory-knowledge.md) — every stored field
- [Configuration reference](configuration.md) — `RSS_SCHEDULE_HOURS`, `RSS_MAX_ARTICLES_PER_FEED`, `RSS_MAX_DIGEST_ENTRIES`, `RSS_REQUIRE_LICENCE`, `MAX_KNOWLEDGE_AGE_DAYS`, and friends
