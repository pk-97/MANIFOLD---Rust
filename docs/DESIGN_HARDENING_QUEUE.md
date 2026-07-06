# Design Hardening Queue — seams that need a Fable review pass before execution

**Status: LIVING · empty as of 2026-07-06 · Owner: Peter → Fable design-review agent.**

Items where an execution worker hit a design-doc gap — the doc contradicts the shipped
code, or a refactor phase lacks the `§6` seam brief (old→new signatures) that
`DESIGN_DOC_STANDARD.md` requires — and **STOPPED clean** (zero code, zero commits) per
the escalate-don't-adapt rule, rather than improvising a shortcut.

Each entry is a **design decision, not an execution task.** Bring it to a Fable agent
for the proper options pass, then fold the answer back into the named design doc as an
addendum with committed signatures, and re-issue the blocked phase. **No item here gets
a "just make it compile" workaround** (Peter, 2026-07-03: *"I don't want hacks or
shortcuts just to get the work done. Everything needs to be done properly."*).

Per item: the gap · evidence (`file:line`) · the decision(s) needed · leans (for the
reviewer to weigh, **not** adopt) · the forbidden shortcut a rushed executor would take.

---

*(No open items.)*

## Resolved

- **2026-07-06 — MEDIA_BACKEND_DESIGN §3** (parked 2026-07-03): decoder trait
  re-committed against the shipped async decode protocol. Answer written as
  `MEDIA_BACKEND_DESIGN.md §3a` (write-into-caller delivery, `MediaBackends` owns the
  shared pool, scheduler payload → neutral `FrameLease`, copy stays on the content
  thread). **P1 re-issuable.**
- **2026-07-06 — MULTI_DISPLAY_DESIGN §6.1** (parked 2026-07-03): per-island state
  seam committed. Answer written as `MULTI_DISPLAY_DESIGN.md §6.1a` (isolation by
  per-`(layer, island)` chain-runtime instances — StateStore key untouched; zero
  primitive edits, the queue's "13 struct-held primitives" inventory was stale after
  the StateStore migration; LED stays legacy until P6). **P2 re-issuable.**

---

## Resolution protocol

1. Fable reviews the item, decides each question, and writes the answer into the named
   design doc as an addendum **with committed signatures** (per `DESIGN_DOC_STANDARD.md`
   §4/§6) — including the old→new seam brief for refactor phases.
2. Remove the item from this queue once the doc carries the decision.
3. Re-issue the blocked phase against the hardened doc.
