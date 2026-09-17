# Stopping a dead end before it runs

`omnimem hook` answers a Claude Code PreToolUse hook. When a tool call proposes
an approach this project has already tried and abandoned, it denies the call and
tells the agent what was abandoned, why, and what worked instead.

## Why this exists

Recall only helps an agent that asks, and agents mostly do not. On the dead-end
benchmark (`scripts/benchmarking/run_deadend_bench.py` in the repository), across
ten runs with the MCP server attached and the answer sitting in the store, the
agent made **zero** memory calls. It read the project's TODO, took the crate that
TODO recommended, and walked into the failure the project had already recorded.

The server's instructions tell it to call `briefing()` at session start. It did
not. Adding more instructions did not change that. What changed the outcome was
catching the proposal at the point of action, which is what this hook does.

## Setting it up

In `.claude/settings.json` in the project, or `~/.claude/settings.json` for every
project:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Edit|MultiEdit|Write|Bash",
        "hooks": [
          {
            "type": "command",
            "command": "omnimem --db /path/to/omnimem.db hook",
            "timeout": 15
          }
        ]
      }
    ]
  }
}
```

`--db` can be left out if `OMNIMEM_DB` is set in the environment Claude Code
runs in. The hook reads the store directly rather than going through the server,
so it works whether or not `omnimem serve` is running. SQLite is in WAL mode, so
reading alongside a running server is safe.

Check it end to end:

```
echo '{"tool_name":"Edit","tool_input":{"new_string":"use some_abandoned_crate;"}}' \
  | omnimem --db /path/to/omnimem.db hook
```

Nothing printed means the call would be allowed. A JSON object with
`"permissionDecision": "deny"` means it would be stopped, and the
`permissionDecisionReason` is what the agent reads.

## What it will and will not block

It only guards tools that change something: `Edit`, `MultiEdit`, `Write`,
`NotebookEdit`, and `Bash`. `Read`, `Glob` and `Grep` are never blocked. A Bash
command is guarded only when it mutates: `ls vendor/kestrel-rs` is investigation
and passes, `cargo add kestrel-rs` is a proposal and is checked.

It reads only what a call would put in place. For an `Edit` that is
`new_string`, never `old_string`, so removing an abandoned approach from a file
is never mistaken for proposing it.

It fails open. If the store is missing, locked beyond its timeout, or anything
else goes wrong, the hook prints nothing to stdout, logs to stderr, and the tool
call proceeds. A memory system that blocks work when it is unwell is worse than
one that forgets.

## What gets blocked

Whatever is in the graveyard: approaches recorded through `record_experience`
with `abandoned_approaches`, or through `log_abandoned`. The matching is the same
keyword scan `warn_if_abandoned` uses, so it tolerates spelling differences
between how a crate is named and how it is written in code (`kestrel-rs` in the
record matches `kestrel_rs` in Rust source).

The refusal text is the same sentence `recall()` shows, built by one function so
the two cannot drift. It carries the breakthrough, the lesson, and how firmly to
take it, which tracks the `effort_score` on the record: something abandoned after
significant effort reads as settled unless nothing else works, while something
tried once reads as worth another look.

## When not to use it

This makes memory authoritative over action, so a wrong or stale record costs
real time. That is a genuine trade, and it is why reads are never blocked: the
agent can always go and check, and the warning says how much weight it deserves
rather than pretending every dead end is permanent.

If a warning is wrong, fix the record rather than removing the hook.
`get_experience` on the memory key shows what is stored, and `record_experience`
can correct it.

## What it is worth

On the dead-end benchmark, with the hook on and the earlier failure in the store,
across five pairs of runs on the same task with the same tools:

* every attempt at the abandoned crate was stopped before it ran (3 of 3)
* no run reached a failing test suite, against 2 of 5 for the control
* median cost fell about 21%, and every pair was cheaper

The sample is small and the control recovered from its dead end quickly, in one
failed test run, so this is a modest saving on a shallow dead end rather than a
headline figure. A dead end that takes longer to discover is worth more.
