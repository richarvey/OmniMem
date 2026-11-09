# OmniMem harness run: test-7.0.10-pairs10

- **Result:** PASSED
- **OmniMem:** 7.0.10 (release)
- **Started:** 2026-11-09T10:56:04+0000  **Finished:** 2026-11-09T11:55:48+0000
- **Suites:** s8, s6, s7, s5, s2, s4, s9
- **Config:** defaults
- **Enrichment:** off

Deterministic suites report counts and failing case IDs, not intervals: their cases are
fixed, so an interval would describe nothing.

| Suite | Status | Passed | Failed | Other | Fixtures | Settle p50 ms |
|---|---|---|---|---|---|---|
| s2 | PASSED | 12 | 0 | – | negative ok, positive ok | 44 |
| s4 | PASSED | 221 | 124 | BY_DESIGN 55 | negative ok, positive ok | 44 |
| s5 | PASSED | 11 | 0 | – | negative ok, positive ok | 44 |
| s6 | PASSED | 8 | 0 | NO_TOMBSTONE 8 | negative ok, positive ok | 44 |
| s7 | PASSED | 20 | 0 | NEAR_MISS_RETURNED 5 | negative ok, positive ok | 44 |
| s8 | PASSED | 150 | 0 | EXPECTED 1 | negative ok, positive ok | 44 |
| s9 | PASSED | 0 | 0 | errors 0, pairs 10 | negative ok, positive ok, seeded ok | – |

## Gates

| Suite | Gate | Held | Detail |
|---|---|---|---|
| s8 | zero cross-project leaks | yes | 0 leaks |
| s6 | every gated cell in the §4.3 table holds | yes | 16 cases |
| s7 | off-topic questions return nothing above the weak band | yes | 0 invented |
| s5 | expired articles are archived by one maintenance pass | yes | all archived |
| s2 | zero STALE after resolution, maintenance and re-entry | yes | no stale value |
| s9 | OmniMem arm passes the suite honestly in every run | yes | 10/10 (control 8/10) |
| s9 | no OmniMem run walks into the dead end | yes | 0/10 (control 8/10) |
| s9 | OmniMem arm uses fewer tokens (median) | yes | -30.6%, fewer in 8 of 10 pairs |

## Claude Code with and without OmniMem (S9)

The same task, 10 pairs on claude-haiku-4-5-20251001: without OmniMem, then with its server, hook and the earlier failure in the store.

| | Without OmniMem | With OmniMem | Change |
|---|---|---|---|
| Median tokens per run | 878,678 | 609,626 | -30.6% |
| Median cost per run | $0.2420 | $0.1084 | -55.2% |
| Runs that walked into the dead end | 8/10 | 0/10 | |
| Runs that sat through failing tests | 6/10 | 0/10 | |
| Passed honestly | 8/10 | 10/10 | |

Fewer tokens with OmniMem in 8 of 10 pairs (one-sided sign test p = 0.055).

## s2

- Partial supersession, bounded validity, equal weight and criterion E need OmniMem to decide, so they wait for the --enrichment configuration (phase 5).

## s4

- 186 supersede calls found no heuristic link, so OmniMem couldn't be told which fact holds; those cases depend on recall ranking alone.
- OmniMem's own query mode (archive/forget by query, needs a score above 0.85) matched nothing 118 times; the adapter then acted on the top recall hit by key.
- Failing cases: adversarial:adv_prefix_collision_19, adversarial:adv_prefix_collision_20, adversarial:adv_prefix_collision_21, adversarial:adv_prefix_collision_22, adversarial:adv_prefix_collision_23, adversarial:adv_prefix_collision_24, adversarial:adv_prefix_collision_28, adversarial:adv_prefix_collision_40, adversarial:adv_paraphrase_supersession_12, adversarial:adv_paraphrase_supersession_13, adversarial:adv_paraphrase_supersession_20, adversarial:adv_paraphrase_supersession_21, adversarial:adv_paraphrase_supersession_28, adversarial:adv_paraphrase_supersession_35, adversarial:adv_paraphrase_supersession_37, adversarial:adv_negation_trap_13, adversarial:adv_negation_trap_15, adversarial:adv_compound_fact_01, adversarial:adv_compound_fact_02, adversarial:adv_compound_fact_03, adversarial:adv_compound_fact_04, adversarial:adv_compound_fact_05, adversarial:adv_compound_fact_06, adversarial:adv_compound_fact_07, adversarial:adv_compound_fact_08, adversarial:adv_compound_fact_09, adversarial:adv_compound_fact_10, adversarial:adv_compound_fact_11, adversarial:adv_compound_fact_12, adversarial:adv_compound_fact_13, adversarial:adv_compound_fact_14, adversarial:adv_compound_fact_15, adversarial:adv_compound_fact_16, adversarial:adv_compound_fact_17, adversarial:adv_compound_fact_18, adversarial:adv_compound_fact_19, adversarial:adv_compound_fact_20, adversarial:adv_compound_fact_21, adversarial:adv_compound_fact_22, adversarial:adv_compound_fact_23, adversarial:adv_compound_fact_24, adversarial:adv_compound_fact_25, adversarial:adv_compound_fact_26, adversarial:adv_compound_fact_27, adversarial:adv_compound_fact_28, adversarial:adv_compound_fact_29, adversarial:adv_compound_fact_30, adversarial:adv_compound_fact_31, adversarial:adv_compound_fact_32, adversarial:adv_compound_fact_33, adversarial:adv_compound_fact_34, adversarial:adv_compound_fact_35, adversarial:adv_compound_fact_36, adversarial:adv_compound_fact_37, adversarial:adv_compound_fact_38, adversarial:adv_compound_fact_39, adversarial:adv_compound_fact_40, adversarial:adv_identifier_obfuscation_03, adversarial:adv_identifier_obfuscation_04, adversarial:adv_identifier_obfuscation_05, adversarial:adv_identifier_obfuscation_06, adversarial:adv_identifier_obfuscation_07, adversarial:adv_identifier_obfuscation_09, adversarial:adv_identifier_obfuscation_11, adversarial:adv_identifier_obfuscation_12, adversarial:adv_identifier_obfuscation_13, adversarial:adv_identifier_obfuscation_14, adversarial:adv_identifier_obfuscation_15, adversarial:adv_identifier_obfuscation_16, adversarial:adv_identifier_obfuscation_19, adversarial:adv_identifier_obfuscation_21, adversarial:adv_identifier_obfuscation_22, adversarial:adv_identifier_obfuscation_23, adversarial:adv_identifier_obfuscation_24, adversarial:adv_identifier_obfuscation_25, adversarial:adv_identifier_obfuscation_26, adversarial:adv_identifier_obfuscation_27, adversarial:adv_identifier_obfuscation_28, adversarial:adv_identifier_obfuscation_30, adversarial:adv_identifier_obfuscation_31, adversarial:adv_identifier_obfuscation_32, adversarial:adv_identifier_obfuscation_33, adversarial:adv_identifier_obfuscation_35, adversarial:adv_identifier_obfuscation_36, adversarial:adv_identifier_obfuscation_37, adversarial:adv_identifier_obfuscation_38, adversarial:adv_cross_lingual_identifier_01, adversarial:adv_cross_lingual_identifier_02, adversarial:adv_cross_lingual_identifier_03, adversarial:adv_cross_lingual_identifier_04, adversarial:adv_cross_lingual_identifier_05, adversarial:adv_cross_lingual_identifier_06, adversarial:adv_cross_lingual_identifier_07, adversarial:adv_cross_lingual_identifier_08, adversarial:adv_cross_lingual_identifier_09, adversarial:adv_cross_lingual_identifier_10, adversarial:adv_cross_lingual_identifier_11, adversarial:adv_cross_lingual_identifier_12, adversarial:adv_cross_lingual_identifier_13, adversarial:adv_cross_lingual_identifier_14, adversarial:adv_cross_lingual_identifier_15, adversarial:adv_cross_lingual_identifier_17, adversarial:adv_cross_lingual_identifier_19, adversarial:adv_cross_lingual_identifier_20, adversarial:adv_cross_lingual_identifier_21, adversarial:adv_cross_lingual_identifier_22, adversarial:adv_cross_lingual_identifier_24, adversarial:adv_cross_lingual_identifier_23, adversarial:adv_cross_lingual_identifier_25, adversarial:adv_cross_lingual_identifier_26, adversarial:adv_cross_lingual_identifier_27, adversarial:adv_cross_lingual_identifier_28, adversarial:adv_cross_lingual_identifier_29, adversarial:adv_cross_lingual_identifier_31, adversarial:adv_cross_lingual_identifier_32, adversarial:adv_cross_lingual_identifier_33, adversarial:adv_cross_lingual_identifier_34, adversarial:adv_cross_lingual_identifier_37, adversarial:adv_cross_lingual_identifier_38, adversarial:adv_cross_lingual_identifier_39, adversarial:adv_cross_lingual_identifier_40, adversarial:adv_recursive_supersession_10, adversarial:adv_recursive_supersession_17, adversarial:adv_recursive_supersession_18

## s6

- Arm F: 8 of 8 forgotten values came back after being fed in again (NO_TOMBSTONE: forget removes the row, and only suppress_topic stops a value).
- Steps 11-13: a backup taken before the forget still held the value in 16 of 16 cases (restore_from_file restores what a backup holds).
- The briefing never showed a suppressed value, so its EXPECTED_GAP may be closed.

## s9

- Median tokens -30.6%, median cost -55.2%, with OmniMem against without; fewer tokens in 8 of 10 pairs (one-sided sign test p = 0.055).
- Walked into the dead end: control 8/10, OmniMem 0/10. Agent model claude-haiku-4-5-20251001.
- Memory tool calls in the OmniMem arm: 0. The hook does the work; the agent rarely calls OmniMem itself, so this measures the hook, not recall.
- One scenario, one model, a shallow dead end: a dead end that takes longer to find is worth more.
