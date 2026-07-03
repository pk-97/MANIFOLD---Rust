# Eval set — labeled sessions for replay tuning

Ground truth for the go-live gates in `../DESIGN.md` §4. Two kinds of labels, both
in `labels.jsonl`, one JSON object per line.

**Incident labels** mark a real historical session where the user had to correct
the assistant's working behavior. The user's correction message is the marker; the
test is counterfactual: replaying the transcript through the observer, a flag of
the expected family must fire *before* the marker message. That is the whole
point of the system — the substrate catches what the user otherwise catches.

```json
{"session": "<uuid>.jsonl", "kind": "incident",
 "marker": "<verbatim prefix of the user's correction message>",
 "expect_family": "anchor/thrash",
 "accept_also": ["coaching/altitude"],
 "notes": "<one line: what the assistant was doing>"}
```

- `expect_family` — the move that best names the drift. A firing from
  `accept_also` counts as a hit too; drift families overlap and the gate should
  not punish a defensible neighbor flag.
- Recall gate: expected-family (or accepted) flag before the marker in ≥ 60% of
  incidents.

**Clean labels** mark sessions with substantial work and no behavioral
corrections. Gate: < 1 flag per clean session on average. Any flag on a clean
session must be hand-reviewed during tuning — some will be genuine drift the user
merely tolerated, and those get re-labeled as incidents, not counted as noise.

```json
{"session": "<uuid>.jsonl", "kind": "clean", "notes": "<one line>"}
```

**Rules for labelers** (human or model): judge only from the transcript, not
hindsight about the eventual fix; label the drift where it *began*, not where it
became obvious; when no move family fits, do not force one — an incident with
`expect_family: null` is still useful as a rubric-gap record. Labels are judgment
artifacts — the biggest available model writes or reviews them (Fable seeded this
set 2026-07-03; extensions should be spot-checked at the same tier).
