# OmniMem hooks for Claude Code

`omnimem hook` answers Claude Code hooks, so memory works without the agent
choosing to use it. Three events:

| event | what it does | writes? |
|---|---|---|
| `pre-tool-use` (default) | denies a tool call that repeats a known dead end | no |
| `session-start` | injects what the project already knows as opening context | no |
| `session-end` | records what the session did, if it changed anything | yes |

OpenCode gets the first two through a plugin that runs the same command; see
[guides/opencode.md](../guides/opencode.md#hooks-stopping-known-dead-ends).
Codex speaks the same protocol, so `omnimem install-hooks --codex` puts the
first two into its `hooks.json` unchanged; see
[guides/codex.md](../guides/codex.md#hooks-stopping-known-dead-ends).
Mistral Vibe runs the same command from its `hooks.toml`, with `--agent vibe`
so the answer is in Vibe's form; see
[guides/vibe.md](../guides/vibe.md#hooks-stopping-known-dead-ends).

## Why these exist

Recall only helps an agent that asks, and agents mostly do not. On the dead-end
benchmark (`scripts/benchmarking/run_deadend_bench.py`, on the `main` branch), across twenty runs with
the MCP server attached and the answer sitting in the store, the agent made
**zero** memory calls. It read the project's TODO, took the crate that TODO
recommended, and walked into the failure the project had already recorded. The
server's instructions tell it to call `briefing()` at session start. It did not.
Adding more instructions did not change that.

What changed the outcome was not asking the agent to remember, but putting the
memory where it could not be skipped: in the opening context, and in the way of
the action.

## Setting it up

```
omnimem install-hooks             # every project: ~/.claude/settings.json
omnimem install-hooks --project   # this project only: ./.claude/settings.json
```

It adds the three entries below and leaves everything else in the file as it
was, keeping a copy of the old file beside it as `settings.json.omnimem-backup`.
An event that already runs an `omnimem hook` is left alone, so running it again
changes nothing. `--dry-run` shows what it would add. The hooks call `omnimem`
when that's what your PATH finds, and the full path to the binary otherwise (the
macOS app's is inside its bundle). Put `--db <file>` before `install-hooks` to
point the hooks at another database. The Flatpak can't reach Claude Code's
settings from its sandbox, so add them by hand there.

By hand, in `~/.claude/settings.json` for every project, or `.claude/settings.json`
in one project:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          { "type": "command", "command": "omnimem hook session-start", "timeout": 15 }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "Edit|MultiEdit|Write|Bash",
        "hooks": [
          { "type": "command", "command": "omnimem hook", "timeout": 15 }
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          { "type": "command", "command": "omnimem hook session-end", "timeout": 30 }
        ]
      }
    ]
  }
}
```

The hooks use the same database as the install on the machine: the desktop
app's, or the server package's on a Linux server (which Claude Code then has
to run as a user able to read and write). To use another, set `OMNIMEM_DB` in
the environment Claude Code runs in, or pass `--db /path/to/omnimem.db` before
the subcommand. A hook that finds no database says so in the hook log and lets
the work carry on. The hooks read the store
directly rather than going through the server, so they work whether or not
`omnimem serve` is running. SQLite is in WAL mode, so this is safe alongside a
running server.

**The `timeout` on SessionEnd is not optional.** Claude Code gives all SessionEnd
hooks a shared budget of 1.5 seconds by default, and raises it to match the
longest configured timeout. Writing a memory has to load the embedding model,
which does not fit in 1.5s.

Which project a hook speaks for: `--project`, else `OMNIMEM_PROJECT`, else the
working directory's name, which is the same default the server's instructions
give an agent, so the two agree without being told twice.

## session-start

Prints the project's briefing as `additionalContext`, which Claude Code prepends
to the session. The graveyard leads, because it is the part that changes what
the agent does next, followed by the current state and hard-won breakthroughs.

It is **read-only**, which is why it does not simply call `briefing()`.
`briefing()` counts its calls and every so often runs dedup and a contradiction
scan; both write and embed. A hook fires on every session, including ones about
to be abandoned, and has no business advancing a maintenance schedule or
stalling a session start behind a model load. `session_briefing()` is the same
aggregate without those parts. Measured at about 6ms.

If a project has nothing recorded, the hook prints nothing. Injecting a heading
with nothing under it into every session teaches an agent to skim past the whole
thing.

The injected text ends by saying the memory may be out of date and inviting the
agent to check. That is deliberate: see the trade-off below.

## pre-tool-use

Reads the tool call on stdin and denies it if it proposes an approach the
project already abandoned, handing back what was abandoned, why, and what worked
instead. A refusal that says only "not that" leaves the agent to rediscover the
answer, which is the work the graveyard exists to save.

It only guards tools that change something: `Edit`, `MultiEdit`, `Write`,
`NotebookEdit`, `Bash`. `Read`, `Glob` and `Grep` are never blocked, and a Bash
command is checked only when it mutates, so `ls vendor/kestrel-rs` passes and
`cargo add kestrel-rs` does not. It reads only what a call would put in place:
for an `Edit` that is `new_string`, never `old_string`, so removing an abandoned
approach is never mistaken for proposing it.

It works out which project it is speaking for the same way session start does
(`--project`, then `OMNIMEM_PROJECT`, then the directory name), and by default
only denies for dead ends recorded on that project, or recorded with no project.
With `GRAVEYARD_SCOPE=all` it also denies for another project's, and the refusal
names the project rather than saying it was tried here.

No embedding: the graveyard scan is keyword-based, so a hook that runs on every
tool call costs about 5ms rather than the second a model load would add.

Check it:

```
echo '{"tool_name":"Edit","tool_input":{"new_string":"use some_abandoned_crate;"}}' \
  | omnimem hook
```

Nothing printed means the call proceeds. A JSON object with
`"permissionDecision": "deny"` means it would be stopped.

## session-end

Reads the session transcript and writes one episodic memory tagged
`session-log`, recording what was asked, the agent's closing account of what it
did, and which files changed.

What it will not do:

* **It will not record a session that changed nothing.** No edits, or no closing
  message, and nothing is written. A session that only read, or that was
  abandoned after two messages, has nothing worth keeping, and storing one
  anyway is how a store fills with noise that outranks memories somebody meant
  to keep.
* **It will not record the same session twice.** SessionEnd can fire more than
  once (a `/clear` then an exit), and a duplicate doubles that session's weight
  in every later recall. A marker key makes it write once.
* **It will not store credentials.** A transcript is exactly where secrets
  surface: a command that printed a token, a config dump, a pasted key. Known
  credential shapes are redacted before anything is written, because recall
  would otherwise put a live secret back into a later session's context. The
  redaction is deliberately blunt, and a false positive costs one redacted line.

What it cannot promise: Claude Code does not guarantee SessionEnd fires on a
crash, a `SIGKILL`, or a closed terminal, and does not promise the transcript is
fully flushed when it does fire. The transcript is therefore read defensively, a
half-written line is skipped rather than costing the record, and **a session that
ends abruptly leaves nothing behind**. Anything that matters should still be
recorded deliberately with `remember()` and `record_experience()`. This is a
safety net, not a substitute.

The quality of the record depends on the agent's closing message, which is
usually its own account of the work and better than anything this hook could
assemble mechanically, but is sometimes thin.

## The trade-off worth understanding

`pre-tool-use` makes memory authoritative over action, so a wrong or stale record
costs real time. That is a genuine cost, and it is why reads are never blocked:
the agent can always go and check, and warnings say how much weight they deserve,
tracking the `effort_score` on the record. Something abandoned after significant
effort reads as settled unless nothing else works; something tried once reads as
worth another look.

If a warning is wrong, fix the record rather than removing the hook.
`get_experience` on the memory key shows what is stored, and `record_experience`
can correct it.

All three fail open. Any error, any unreadable store, and the hook prints
nothing and work proceeds. A memory system that blocks work when it is unwell is
worse than one that forgets.

## What it is worth

On the dead-end benchmark (the test harness's S9, against the 7.0.10 release on
9 November 2026), ten pairs of runs on the same task with the same tools and
`claude-haiku-4-5-20251001`, one without OmniMem and one with its server, the
hook and the earlier failure in the store:

* the control walked into the dead end in 8 of 10 runs, the OmniMem arm in none
* 6 of the control's runs sat through a failing test suite, and none of OmniMem's
* the OmniMem arm passed honestly in 10 of 10, the control in 8 of 10
* median tokens per run fell 30.6% (fewer in 8 of 10 pairs, one-sided sign test
  p = 0.055) and median cost 55.2%

The run, with every per-run figure, is in
[`scripts/benchmarking/results/7.0.10-harness-2026-11-09`](../scripts/benchmarking/results/7.0.10-harness-2026-11-09/report.md).

The agent made no memory tool calls in the OmniMem arm: the hook did the work,
so this measures the hook, not recall. The sample is small, it's one scenario on
one model, and the dead end is a shallow one. A dead end that takes longer to
discover is worth more, and that isn't measured here.

`session-start` and `session-end` have **not** been through the benchmark. The
figures above are for the PreToolUse guard only.
