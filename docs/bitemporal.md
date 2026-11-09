# Bi-temporal memory

Since 7.0.11 every memory says two separate things about time: when the thing it records was true, and when OmniMem believed it. They sound alike, and most of the time they are, but they come apart the moment something changes.

Say you told OmniMem in January that Sam lives in Berlin. In May you tell it Sam moved to Lisbon on 1 April. Now there are two different questions you might ask:

- **Where did Sam live last March?** Berlin. That's a question about the world.
- **What did we think in mid-April?** Also Berlin, but for a different reason: the move hadn't been recorded yet. That's a question about OmniMem.

Before 7.0.11 neither question had an answer. A memory had a `created_at` and an `updated_at` that each change overwrote, so correcting a fact quietly lost when the wrong value was in force.

## The two intervals

| Field | What it says | Empty means |
|---|---|---|
| `valid_from` | When it became true | True from when it was recorded (`created_at`) |
| `valid_to` | When it stopped being true | Still true |
| `created_at` | When OmniMem recorded it | (always set) |
| `retired_at` | When OmniMem stopped believing it | Still believed |

Valid time is `[valid_from, valid_to)` and record time is `[created_at, retired_at)`. Both are half-open, so a memory that ends on 1 April isn't true on 1 April.

`event_date` hasn't changed. It's still the date an event happened, used to boost date-shaped queries, and it's separate from validity: "the release went out on 3 March" has an event date but is true for ever.

## Who sets them

You don't have to do anything for most memories. Nearly everything is true from when it's recorded until further notice, and the empty defaults say exactly that.

- **`remember` points out a likely supersession.** When the new memory says something changed ("moved to Lisbon", "switched to Caddy", "now at 10.0.0.9") and a close neighbour is still believed, the result carries `may_supersede` with the neighbour's key. It's a hint for the agent to put to you; nothing closes until a `supersedes` resolution is accepted.
- **`end_validity`** ends a memory's validity when nothing replaces it: "we stopped using Traefik" has no newer memory to supersede it with. `valid_to` defaults to now. It doesn't change whether the memory is believed (it was true, it just isn't any more), so it needs no approval, and the facts extracted from it follow.
- **`remember`** takes `valid_from` and `valid_to`, as an ISO date (`2026-04-01`), a timestamp, or a phrase recall would understand ("last March", "3 weeks ago"). Use `valid_from` when something became true before you recorded it, and `valid_to` when you're recording something that has already ended.
- **Resolving a contradiction** with `supersedes` ends the loser's validity when the winner says it began (its `valid_from`), or now if it doesn't say, and retires the loser. A loser that already had an earlier end keeps it. In the example, settling "moved to Lisbon" over "lives in Berlin" closes Berlin at 1 April.
- **`both_wrong`** retires both memories but ends neither's validity: they were never true, so there's nothing to end.
- **`reopened`** undoes the resolution it reopens. A memory that resolution superseded or rejected goes back to `disputed`, its `retired_at` is cleared, and the end of validity it wrote is taken back. The resolution's `history` keeps what was decided before.
- **A person rejecting a memory** (in the settings window or with `omnimem trust reject`) retires it, and restoring it un-retires it. `retired_at` follows trust wherever trust moves.
- **Extracted facts** take their dates from the text when it gives them ("since April"), and otherwise their source's `valid_from`, `valid_to` and `retired_at`. A fact that took its source's end loses it again if the source's is cleared, but one with an end of its own keeps it.

Every change to an interval is in the audit log at `lifecycle`, with the old and new values.

A memory older than 7.0.11 needs no rewrite. Schema 6 reads a superseded or rejected record with no `retired_at` as retired at its `updated_at` (which a resolution bumps as it supersedes), and a superseded one as no longer true from then. The migration writes the exact time from the resolution where there is one, and leaves `valid_from` empty everywhere: nobody knows it, and a made-up date would be worse than none.

## Asking about a time

`recall` and `recall_index` take two optional parameters.

- **`valid_at`** answers "what was true then": memories whose valid time includes it.
- **`known_at`** answers "what did we believe then": memories recorded by then and not yet retired, with trust as it stood. A memory rejected or superseded after that time reads as believed, and carries its `retired_at` and `superseded_by` so you can see what happened to it later.

Both together ask what OmniMem believed then about then. An end of validity recorded after `known_at` doesn't count, because OmniMem didn't know it yet. That's what `valid_to_recorded_at`, written beside every `valid_to` OmniMem writes, is for.

With the Berlin and Lisbon memories:

| Call | Answer | Why |
|---|---|---|
| `recall("where does Sam live", valid_at="2026-03-01")` | Berlin | Lisbon only became true on 1 April |
| `recall(..., valid_at="2026-05-01")` | Lisbon | Berlin ended on 1 April |
| `recall(..., known_at="2026-04-15")` | Berlin, `trust: accepted` | The move was recorded in May |
| `recall(..., valid_at="2026-04-15", known_at="2026-04-15")` | Berlin | In mid-April we didn't know Berlin had ended |
| `recall(...)` | Lisbon, with Berlin under `replaces` | The ordinary answer |

A hit carries `valid_from`, `valid_to` and `retired_at` as ISO times wherever it has them. A memory with none of them reads exactly as it did before.

Dead-end warnings ignore both parameters. A dead end is experience, not a claim about the world, so it warns whatever time you ask about.

## An ordinary recall

Without either parameter, recall answers for now. A memory whose `valid_to` has passed still comes back, ranked at `VALIDITY_EXPIRED_WEIGHT` (0.8 by default) of its score and carrying its `valid_to`, so it can't be mistaken for something true today. A superseded memory is folded into the memory that replaced it, as before.

The briefing and the skill compiler only use memories that are still true, the same way they only use believed ones.

None of this moves anything for a memory with no `valid_to`. That covers every memory from 6.x and from 7.x before 7.0.11, so an upgrade changes no scores until something has an end.

## In the settings window and the CLI

Each memory's page has a **When it was true** section: when it became true and stopped, when OmniMem recorded it and, if it has, when it stopped believing it. Two date fields set or clear either end of validity (an empty field means "from when it was recorded" or "still true"), with an optional reason. The change is audited as the panel's, and the facts extracted from the memory follow it. The Search page has **True on** and **Known on** date fields, which are `valid_at` and `known_at`, and a result that has stopped being true says since when.

`omnimem search` takes the same two, for a look at the raw store:

```bash
omnimem search "where does Sam live" --valid-at 2026-03-01
omnimem search "where does Sam live" --known-at 2026-04-15 --json
```

`explain_memory` and `recall_detail` show `valid_from`, `valid_to`, `recorded_at` (the memory's `created_at`) and `retired_at` as ISO times, leaving out an end the memory doesn't have.

## What forget means for history

`forget` is still erasure. A forgotten memory has no record time because it no longer exists, and the audit log loses its text too. That's deliberate: when someone asks for something to be deleted, privacy beats history. If you want a memory to stop counting but keep the record of it, reject it or supersede it instead: it stays in the store, and a `known_at` from before then still finds it.

## Limits

- `known_at` can only see as far back as the record goes. Since 7.0.11 the audit log keeps every row by default (`AUDIT_RETENTION_DAYS=0`), so nothing ages out unless you set a retention.
- When a dispute began isn't recorded, so under `known_at` a memory that's disputed now reads as disputed then too.
- Archiving and deprioritising aren't timed, so `known_at` judges a memory's state as it is now, not as it was.
- If the memory that superseded another is deleted, the loser is believed again (its `retired_at` clears) but keeps the end of validity the supersession gave it.
- Since 7.0.12, `end_validity(key, valid_to?, reason?)` ends a memory's validity when nothing replaces it ("we stopped using Traefik"). It leaves trust alone, needs no approval, is audited and bumps `updated_at`.
