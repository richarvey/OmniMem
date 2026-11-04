# The audit log

Memories change. An agent archives one, a feed expires another, you retag a third from the settings window, and a week later you want to know who did that. Until 7.0.6 the answer was "nobody wrote it down": a change bumped `updated_at` and left a line in the process log, and once a memory was deleted there was nothing to look at.

The audit log is a record of changes, held in the same database. Since 7.0.7 it's on by default at `lifecycle`: who deprioritised, archived, reinstated or forgot what, and why, with no memory text. 7.0.6 shipped it off; an install that set `AUDIT_LOG=off` keeps it off, and only an unset value changed.

## Choosing a level

Set `AUDIT_LOG` on the Configuration page, in `omnimem.env`, or in the environment. It takes four levels, and a value that isn't one of them is treated as `lifecycle`, with a warning in the log, so a typo never switches the log off and never starts one that holds text.

| Level | What it records |
|---|---|
| `off` | Nothing |
| `lifecycle` | The default. State changes and deletes: deprioritise, archive, reinstate, forget, the project-wide versions of those, maintenance archives, suppressing and unsuppressing a topic, resolving a contradiction (the choice and the winner, never the memories' text), and skill writes. When a memory's state changes, the facts extracted from it follow it, and that is one entry against the source listing the facts that moved |
| `writes` | Everything in `lifecycle`, plus every other write: new memories (key, namespace, project and tags, but not what they say), retags, licence and provenance changes, contradiction links, extracted facts, feed items, restores, project context edits, experience records |
| `full` | Everything in `writes`, plus the text of a memory before and after each change |

`AUDIT_RETENTION_DAYS` says how long rows are kept. The default is 90, and 0 keeps them for ever. The maintenance pass prunes the old ones, and so does starting OmniMem, so a log you've switched off still ages out.

A `lifecycle` row is about 340 bytes on disk, indexes included (measured from 6,000 deprioritise and archive rows). A busy store that sees 50 state changes a day writes about 6 MB a year, and with the default 90 days it holds about 1.5 MB at any time. A plain `remember` writes no `lifecycle` row, so the log grows with what happens to memories, not with how many you store.

### Privacy at `full`

`full` is the only level that stores what your memories say. Every edit leaves the old text and the new text in the `audit` table, so rewriting a memory doesn't remove the old version until its rows age out: keeping that history is what `full` is for. Anyone who can read the database file can read them, and so can anyone who has a backup made with the audit log in it. If you keep OmniMem on a shared machine, use `writes`.

Forgetting is different. Since 7.0.7, `forget` (and `delete_project`) scrubs the text from the log in the same transaction as the delete: every earlier row for that memory, and for the facts extracted from it, loses the fields only `full` keeps, and a contradiction link on another memory that points at it loses the model's explanation, which may quote it. The rows stay and still say who did what, when, and why, with the memory's state, tags and project, so the log shows *that* something was forgotten without keeping *what* it said. The forget's own row never holds text at any level, and says how many rows it scrubbed (`scrubbed: 3`). The scrub runs whatever the level is now, because rows written at `full` outlive a move down to `lifecycle` or `off`. A backup made with the audit log before the forget still has the text, so treat those as you would any copy of the database.

The lower levels never hold memory text. They record keys, states, tags, projects, licences and provenance, and the reasons callers gave. A change to a field they don't show is listed by name (`other_fields: ["content"]`), never by value.

## What a row holds

| Field | Contents |
|---|---|
| `at` | When it happened |
| `actor` | `mcp`, `panel`, `hook`, `maintenance`, `enrichment`, `rss`, `cli` or `import` |
| `client` | For `mcp`, the name the client gave when it connected (`claude-code`), with the OAuth client id in brackets if it signed in with OAuth. Empty for everyone else |
| `action` | `remember`, `transition`, `forget`, `retag`, `set_licence`, `set_provenance`, `suppress`, `unsuppress`, `contradiction_link`, `contradiction_resolve`, `skill_write`, `restore`, `extract_fact`, `feed_item`, `cascade`, `project_transition`, `project_delete`, `update` and a few more |
| `key` | The memory, project or skill key, or the topic for a suppression |
| `from`, `to` | The fields that changed, with their old and new values |
| `reason` | The reason the caller gave, where there was one (an archive or deprioritise reason, a suppression reason) |

A row is written in the same transaction as the change it records. If the change fails, the row isn't there, and if the row can't be written, neither is the change.

One thing to know: a call that makes several writes records each as it goes. Deprioritising a memory, for instance, is one entry for the memory and, if it has extracted facts, one more listing them. Each is atomic, but the two aren't one transaction.

## Reading it

**The settings window.** Audit Log in the sidebar lists the log newest first, with filters for key, actor, action and date (From and To take `2026-11-03` or `7d`). Each memory's page has a History section with its own rows.

**The command line**, for a server with no window:

```bash
omnimem audit                              # the 50 most recent rows
omnimem audit --key mem:episodic:01J...    # one memory's history
omnimem audit --since 7d --actor mcp       # what agents did this week
omnimem audit --action forget --json       # one JSON object per line
```

`--since` takes `30m`, `12h`, `7d`, `2w`, a date (`2026-11-03`) or a date and time (`2026-11-03T14:30`, UTC). The command reads the database itself, so it works whether or not the server is running; run it as the service's user on a server install (`sudo -u omnimem omnimem audit`).

**`explain_memory`.** When the log is on and has rows for a memory, the result gains a `history` list, newest first, up to 20 entries. There's no new tool, so the 48 stay 48, and with the log off the result is the 6.x one.

## Backups

The audit log isn't in a backup unless you ask. It can be big, and at `full` it holds text.

- `omnimem export backup.json --audit`
- Tick **Include audit log** beside Create Backup on the Backups page

`dump_to_file` doesn't take it: it keeps the 6.x parameters. A restore, from the tool, the window or `omnimem import`, adds the rows a backup carries and skips any already in the log, so restoring twice, or onto the store the backup came from, adds nothing.

## Where it doesn't look

Recalls don't write rows (a recall bumps counters on the memories it returns, and those aren't changes), and neither do the tool metrics, recall logs, caches or the enrichment queue. OAuth clients and tokens are never recorded. Nothing secret is logged: a memory's text is held only at `full`, and the session-end hook's redaction has already run by the time a memory reaches the store.
