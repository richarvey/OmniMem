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
```

You can also manage feeds from the [web UI](web-ui.md)'s RSS Feeds page — uploading a new feeds.yml just writes the file and the worker picks up the change automatically.

## How ingestion works

Each article gets fetched, stripped of HTML, summarised to a couple of sentences by Claude Haiku, embedded, and stored in the `knowledge` namespace with an `expires_at` timestamp (default 30 days, configurable via `MAX_KNOWLEDGE_AGE_DAYS`). Articles are labelled with the project `RSS` (or the feed's own `project:` label if you set one) so ingested content stays separable from knowledge captured in conversation — filter by project in the web UI, or pass `project="RSS"` to `recall()` to search only articles. Expired articles are auto-archived during maintenance. Duplicates are skipped by URL. The worker runs once on startup and then on whatever schedule you set in `RSS_SCHEDULE_HOURS`.

If no `ANTHROPIC_API_KEY` is set, the worker still runs — summaries fall back to simple truncation instead of Haiku.

## Keeping articles

If an article turns out to be genuinely useful, call `promote_knowledge(key)` to clear its expiry and keep it permanently — or `promote_knowledge(key, domain="python")` to also feed it into that domain's compiled skill as reference material. See [the skill compiler](skill-compiler.md) for how promoted articles become Reference rules.

## Influencing skills

Promotion vets one article at a time. When a whole feed reliably matters to a skill, give it a standing association instead: a `skills:` mapping (shown above, also editable per feed in the web UI) ties the feed to one or more skill domains with an influence score from 1 to 10. Every recompile of that skill then pulls the feed's latest articles into a Feed watch section automatically — the score is how many recent articles the feed contributes, so 10 dominates and 1 adds a single headline. Feeds without a `skills:` mapping keep today's behaviour: their articles never touch a skill unless you promote one by hand. Details and the review-gate implications are in [the skill compiler](skill-compiler.md#feed-influence).

## See also

- [Knowledge memory spec](memory-knowledge.md) — every stored field
- [Configuration reference](configuration.md) — `RSS_SCHEDULE_HOURS`, `RSS_MAX_ARTICLES_PER_FEED`, `RSS_MAX_DIGEST_ENTRIES`, `MAX_KNOWLEDGE_AGE_DAYS`, and friends
