# OmniMem v7: changes for Mycelium conformance

**Status**: reconstructed, normative for OmniMem 7.0.

The original `omnimem-v7-change-spec.md` (drafted August 2026 alongside the Mycelium protocol, coordination server and daemon specs) is not in this repository or on the development machine. This document rebuilds it from the decisions recorded in OmniMem when the spec was written, and settles one value the record left open. Where the recorded decisions and this document disagree, the recorded decisions win until someone with the original says otherwise.

Sources, all in the `omnimem` and `mycelium` projects of the maintainer's OmniMem store:

- `mem:project:01M03VXX8K6XYCNFSYEXX0RRR0`: OmniMem v7 changes for Mycelium conformance (August 2026)
- `mem:project:01M03VX68Q7Q1N30H0V316GFQB`: Mycelium memory vocabulary and wire format (August 2026)
- `mem:project:01M1EB43H4B3NWS60GVSJMX0QY`: cross-version constraints between the 6.6.x schema work and this spec (September 2026)

The spec was written against 6.5.1. Since then 6.6.1 shipped `licence`, 6.6.2 shipped `provenance` and 6.7 changed the embedding backend, so this version is written against 6.7.1.

## The line this spec holds

**OmniMem knows about memories. The daemon knows about the network.**

OmniMem gains no knowledge of the routing embedding space or its version, no network transport, no presence or leases, and no audit emission. Any future change that would put a routing-space version or a peer node id inside OmniMem is a sign the line has moved the wrong way.

OmniMem is the reference implementation: the Mycelium vocabulary is derived from its record shape, and other memory backends conform to it rather than the reverse.

## 1. Schema

Four fields join every writable namespace. They are properties of the memory, not of the network: a memory should know what it is and who wrote it whether or not Mycelium exists, so OmniMem stores them natively rather than the daemon synthesising them at publish time.

| Field | Type | Rules |
|---|---|---|
| `origin_id` | ULID | Set once at creation, immutable, preserved by custodians |
| `content_hash` | `sha256:` + lowercase hex | Computed on write, recomputed on any content mutation, **indexed** |
| `epoch` | u64 | Starts at 1, monotonic per `(origin_id, content_hash)` |
| `classification` | `{level, scopes: [string]}` | `level` ∈ `public`, `internal`, `confidential`, `restricted`. **Provisional**: the governance model is unsettled |

`content_hash` must be indexed (a Valkey secondary index). Custodian freshness checks and collapse look records up by hash and must never scan.

### Content hash derivation

Byte-identical across every implementation, because the daemon runs on Windows, macOS and Linux and a custodian must be able to verify content that originated on a different platform:

1. The `content` field only
2. Unicode NFC
3. Line endings to LF
4. Strip trailing whitespace from each line
5. Strip leading and trailing blank lines
6. Encode UTF-8
7. SHA-256, lowercase hex, prefixed `sha256:`

Accepted trade-off: a whitespace-only edit does not change the hash. That is right for prose memories and would be wrong if content were ever code where indentation carries meaning.

The collapse key on the network is `(origin_id, content_hash)`, never the hash alone. Two origins that independently wrote identical text are two corroborating sources.

### Implementation warning

This spec was written against 6.x, where filtering went through valkey-search: backslash-escaped or quoted tag values matched nothing and in-brace alternation `{a|b}` returned an empty set. None of it applies to 7.0, where filters are SQL and `content_hash` is an ordinary SQLite index (`crates/omnimem-store/src/schema.rs`). Kept because the 6.x branches still need it.

## 2. Classification is not licence

These answer different questions and must never be read as each other.

- **`classification`** (this spec): who may *see* a memory inside the Mycelium network. Governs cluster emission and the most-restrictive roll-up below.
- **`licence`** (6.6.1): whether the content may be *redistributed* outside the organisation.

A summary of a paywalled standard can be `classification.level = public` (no disclosure risk, it is a public article) and `licence = restricted` (no right to sell a derivative). Reading `public` as permission to ship is exactly the retroactive liability the licence field exists to prevent.

## 3. `cluster_profile`

**OmniMem clusters, the daemon embeds.** A centroid is a mean vector in OmniMem's native embedding space, and a mean vector cannot be re-projected into the routing space: projection needs text. So OmniMem clusters where its vectors already live and emits text; the daemon embeds that text. All embedding-model concern stays in the daemon, and a publish cycle costs at most 512 summary embeddings rather than one per memory.

```
cluster_profile(target_members=40, max_clusters=512, min_members=30) -> [ClusterSummary]
```

`ClusterSummary` carries:

| Field | Meaning |
|---|---|
| `cluster_id` | Stable identifier for the cluster |
| `summary_text` | Short representative text; the daemon embeds this |
| `cluster_label` | Human-readable label |
| `member_count` | Members in the cluster, never below `min_members` |
| `outcome_class` | See §4 |
| `provenance_class` | See §4 |
| `avg_experience` | Mean experience weight of members |
| `classification` | Most restrictive across members |
| `member_states` | Counts of member lifecycle states |

Rules:

- **k-anonymity floor**: clusters below `min_members` are not emitted. A cluster of four is close to being the memories themselves, open to embedding inversion
- Clusters are computed **separately per `outcome_class`**, with `max_clusters` **split proportionally** across classes, not granted to each
- Archived memories are excluded **before** clustering
- Clustering is **deterministic** for a given store, so a republish does not churn the routing index for nothing
- Incremental on write, full recompute on a cadence (full k-means over 25,000 vectors is fine hourly on a laptop, not per write)
- Emit a **drift metric** so the daemon republishes on change rather than on a timer
- **Classification is most-restrictive-across-members**: one confidential member makes the cluster confidential. Blunt and it over-restricts, but the alternative leaks through the existence of a cluster
- `summary_text` must not reproduce any single member verbatim; a summary that is one memory's text defeats the member floor. Redaction from the signed policy bundle is applied before it leaves OmniMem

## 4. Vocabularies

### `provenance_class`

**Normative: `retrieved`, `concluded`, `asserted`**, exactly the values shipped in 6.6.2 as `PROVENANCE_CLASSES` in `mcp_server/memory/provenance.py`, now `crates/omnimem-core/src/classification.rs`.

| Value | Meaning |
|---|---|
| `retrieved` | From an external source: an article, documentation, a page |
| `concluded` | The system's own reasoning or write-up of work done |
| `asserted` | Stated directly by the human |

The recorded spec names the field but not its values. This document settles them on the shipped vocabulary rather than leaving a second, undefined one: a mismatch would stay silent until cluster time, when clusters would be grouped or labelled on values no memory carries. There is no `unknown`. A cluster's `provenance_class` roll-up rule is not yet decided (see Open questions).

Any change to `PROVENANCE_CLASSES` is a change to this spec, and the reverse.

### `outcome_class`

| Memory `outcome` | `outcome_class` |
|---|---|
| `succeeded` | `success` |
| `pivoted` | `success` |
| `abandoned` | `abandoned` |

### Lifecycle state on the network

| `state` | Publish | Serve |
|---|---|---|
| `active` | yes | yes, normal ranking |
| `deprioritised` | yes | yes, with a weight penalty ("less visible", not "wrong") |
| `archived` | never | never |

## 5. Lifecycle: operations that bump `epoch`

- `record_experience`, **when the outcome changes**
- `log_abandoned`

This changes their semantics. Today they are additive annotations; in v7 they become versioned edits, which is the whole mechanism by which a later-abandoned approach reaches custodians. Any caller that assumes they are side-effect-free needs review.

Do **not** bump `epoch`: `retag`, `recall_count` changes, experience-weight drift.

- `forget()` gains a network obligation: emit a revocation record so custodians drop their copies
- `archive()` is **not** revocation: an archived memory stops publishing, but custodians keep existing copies

## 6. Adapter surface to build

- A classification filter on `recall`, applied inside the search. Post-filtering leaks a memory's existence through result counts
- `cluster_profile` (§3)
- A classification accessor
- `freshness`: batch `content_hash` → `epoch`, `state`
- `by_hash`: fetch by hash, for custodian verification
- `explain_memory` exposes `origin_id`, `content_hash`, `epoch` and `classification`

## 7. Migration

Idempotent, re-runnable and interrupt-safe:

- Compute `content_hash` for every memory
- `epoch = 1`
- `origin_id` = the local node
- `classification = {level: internal, scopes: []}`
- Build the hash index
- **Report collisions**: identical content under two keys is expected, not an error, but it is a local dedup opportunity and it affects network collapse

Backfilling `epoch = 1` everywhere means custodians can't tell pre-migration revisions apart. That is acceptable: there are no custodians before v7. No wire compatibility with 6.x is needed, because nothing consumes OmniMem over a network today.

## Open questions

- **Classification governance** is provisional: who sets a level, and whether scopes are free strings or a managed list
- **`provenance_class` roll-up**: whether a cluster takes its members' majority class, the least trusted class present (`concluded` over `retrieved` over `asserted`, mirroring classification's most-restrictive rule), or is clustered separately per class like `outcome_class`
- **Timestamps** on the wire are RFC 3339 UTC; OmniMem stores float-epoch strings, and the adapter converts. Where that conversion lives (OmniMem or the daemon) is not recorded
