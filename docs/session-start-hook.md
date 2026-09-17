# Making an agent ask before it works

A measured problem, and a recipe for it.

## The problem

An agent with OmniMem attached and a populated store will often not query it.
In a benchmark run where the agent had the memory server available and the
answer was sitting in the store at 0.50 relevance, it made **zero memory
calls** and re-derived the answer from the codebase instead. The server
instructions already say to recall before reaching for documentation; that is
advice, and advice did not bind.

Nothing is wrong with the agent's reasoning here. Reading a file in front of
you is concrete and immediate; querying a memory store is a detour that might
return nothing. Left to itself an agent takes the shorter path.

## What does work

A `SessionStart` hook injects context once per session, before any work. It
costs a handful of tokens, intercepts nothing, and forbids nothing.

`.claude/settings.json` in the project:

```json
{
  "hooks": {
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": ".claude/omnimem-start.sh" }] }
    ]
  }
}
```

`.claude/omnimem-start.sh`:

```sh
#!/bin/sh
# Emit guidance for this session. Keep it short: it is paid for on every turn.
cat <<'JSON'
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":
"Before investigating anything in this project, call briefing(project=\"<name>\") to pick up what earlier sessions recorded, and warn_if_abandoned(\"<candidate>\") before committing to any library or approach. Read source only for what the briefing did not already answer."}}
JSON
```

Verified: a `SessionStart` hook does inject context and the injected
instruction is followed. What is **not** yet verified is that this specific
wording makes an agent call `briefing()` in practice. Injection working and
the behaviour changing are different claims, and only the second one matters.
Measure it before relying on it.

## Order matters, and it depends on the project

The instinct is often "read the files, then call briefing". That is backwards
for an established project: the agent has already done the work the briefing
would have saved, which is the duplicated effort the memory layer exists to
remove.

- **Established project** (the store has memories): briefing first, then read
  only what the briefing did not answer.
- **New project** (empty store): briefing returns nothing useful, so reading
  first is the only way to learn anything, and the session ends by recording
  what was learned.

A hook can branch on this by asking the store how much it holds, and emitting
different guidance either way. Keep the query cheap; it runs on every session.

## What not to do

A `PreToolUse` hook can *deny* `Read`, `Grep` and `Glob` outright and redirect
the agent to memory. It works: in testing, a denied read with a reason naming
the alternative sent the agent to `recall()` and it answered correctly in four
turns. A bare denial with no reason made it stall and ask for permission
instead, so the reason string is doing the work, not the denial.

It is still the wrong default. Memory is a claim about the past; the source is
ground truth. Blocking verification makes stale memory authoritative, and the
failure mode is an agent confidently citing a decision about code that has
since changed. If you want it, scope it narrowly: deny only where a recall in
the same session already returned a high-confidence hit covering the question,
and always leave an override.
