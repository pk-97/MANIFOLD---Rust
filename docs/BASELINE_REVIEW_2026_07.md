# Design-Doc Baseline Review — 2026-07-05

<!-- index: Closing report of the 48-doc baseline review (orchestration-quality step 2): method, per-doc verdicts for the build-queue tranche, systemic findings (status rot >> fork ambiguity), decision-list outcome (all closed), and the review-process lessons. -->

**Status:** COMPLETE 2026-07-05 · Fable · part of the orchestration-quality initiative
(step 1 = the verification contract in `DESIGN_DOC_STANDARD.md` §10 + `VERIFICATION_DEBT.md`,
landed `cdf37515`; step 2 = this review).

Trigger: overnight orchestration landings reached the app broken (invisible automation
lanes, glb import misbehavior). Peter's hypothesis: docs carry open-ended design
questions that orchestrators trip on. The review tested that hypothesis against all
48 active design docs.

## Method

- **Long tail (40 docs):** three parallel read-only agents ran a mechanical triage —
  status-line truth, unlabeled-fork hunt, 3-anchor freshness sample, phase-gate shape.
- **Build-queue tranche (8 docs):** main-context deep review — every fork classified
  per the §2 no-unlabeled-forks rule, anchors re-run against the tree, §10 level
  labels added, doc-vs-tree divergences reconciled in place. Docs: UI_AUTOMATION,
  PARAM_STORAGE, TIMELINE_INGEST, AUDIO_SENDS_UX, GAUSSIAN_SPLATS, REALTIME_3D,
  IMPORT, SESSION_MODE.

## The verdict on the hypothesis

**Fork ambiguity is largely a non-problem; status rot is the real disease.** Across
48 docs: ~5 genuinely unlabeled forks (two were Peter-level — both decided today;
two sit in a doc that belongs in the archive; one was internal-inconsistency, fixed).
Against that: **~17 docs carried false or missing status lines**, including fully
shipped work still declaring itself unbuilt (AUTOMATION_LANES said "Not implemented"
after shipping — the exact doc whose in-app breakage started this). A worker reading
"not built" over built code rebuilds it, fights it, or wires it blind — that is the
bug factory, and it is mechanical to fix and mechanical to prevent.

Countermeasures landed: the one-time status-truth pass (17 docs, `1a828b74`) and
Peter's rule as `DESIGN_DOC_STANDARD.md` §8.9 — **a landing is not done until the
design doc's status updates in the same landing.**

## Deep-tranche verdicts (all CLEARED for execution)

| Doc | Verdict + load-bearing findings |
|---|---|
| UI_AUTOMATION | Cleared @ `cc23e0ce`. Caught: automation-lane hit-test surface shipped after the doc was written — added to P1/D5 scope (it's how VD-001 burns down); interact.rs grew ~10× (P2 seam re-derivation now mandatory); dev feature renamed `ui-automation`. Build wave ready — kickoff brief at `.claude/briefs/UI_AUTOMATION_WAVE_KICKOFF.md` (untracked, local). |
| PARAM_STORAGE | P1 had ALREADY shipped (`c7ae831f`) against a "not built" status — status corrected; P1 negative gates re-run and hold; resolver anchors drifted ~−390 lines, re-derivation commands authoritative. P2 (strong-model storage swap) is next and unblocked. |
| TIMELINE_INGEST | Clean — zero unlabeled forks; the AppKit drag-position unknown is a labeled verify-first with priced fallback chain. Anchors: symbol-fresh, line drift only. P1/P2 gates are L4 by nature (no oracle can synthesize OS drags — not even the automation layer). |
| AUDIO_SENDS_UX | Clean, freshest anchors of the tranche (4/4 exact). Fixed: D5 "Source" confirmation hadn't propagated into §2/P4, which still read "blocked". P2–P4 already gate on headless PNGs (L2). |
| GAUSSIAN_SPLATS | Strongest doc reviewed (pixel-assert gates, e.g. sort correctness via camera-flip). Prerequisite `render_scene` P1 re-verified in-tree @ `8daa89fc`; "no GPU sort exists" negative claim re-run, true; camera row pinned (both free_camera + look_at_camera landed). |
| REALTIME_3D | Status was "not built" — actually P0/P1/P4/§9 shipped. **As-built P1 deviation: object transforms NOT port-shadowed yet** (render_scene.rs header) — flagged for P2+. Remaining: P2 shadows, P3 atmosphere, P5–P7 viewport/gizmos/preset. |
| IMPORT | Reality reconciliation: the shipped `node.gltf_mesh_source` (glTF wave) is a mesh-level door, NOT this doc's P1 scene importer — P1 read-back now must reconcile parser placement (same `gltf` crate, so extend-don't-redesign). Held-out fixtures = the three untracked CC0 scans in `tests/fixtures/gltf/` (VD-003). |
| SESSION_MODE | Status honest. P4/P5 briefs are one-liners — fine while P4 is Peter's hands-on phase, MUST be expanded to §5 form before agent delegation. Verified: `ContentState` carries no session play-state yet — P4's snapshot fields are greenfield. |

## Long-tail highlights (full agent output in the session transcript)

- Standard-era docs (post-2026-07-03) are uniformly decisive — the standard works.
- The stale cluster is the June audio/fusion docs: BUFFER_CHAIN_FUSION anchors drifted
  hundreds of lines; NODE_GROUPS_UI points at a file that moved crates.
- Archive candidates (shipped records still dressed as designs): PRIMITIVE_LIBRARY,
  CANVAS_API, CLIP_THUMBNAILS base — deferred to a docs-hygiene pass, needs a
  link-breakage sweep before moving.
- MCP_INTERFACE and CHROME_API have zero file:line anchors (flagged; MCP is greenfield
  so acceptable, CHROME_API should gain them when 2b executes).

## Decision list — CLOSED

Both Peter-level forks decided 2026-07-05, recorded at source with rationale:
1. **Detect concurrency → block/reject** (AUDIO_CLIP_DETECTION §10): button disabled
   with visible "detection busy"; no queue ("so the user knows they can't
   auto-populate multiple audio files at once").
2. **Audio bundling → reference by path, never embed** (AUDIO_LAYER §7/§10, `e1ea6a7b`);
   Collect All and Save ships as the explicit portability action.

The six deep reviews surfaced **zero additional Peter-level forks** — everything else
was executor-level and is now labeled in place.

## Review-process lessons (for the next review session)

- **Verify surprising tool output before it becomes a doc edit.** A `-rln` flag typo
  turned ripgrep's output into a replace-mode display ("everything is named `ln`"),
  which briefly produced a false "free_camera absent" doc edit — caught only because
  the collision hypothesis was re-tested with a clean command. Same discipline as
  §10: an oracle you haven't sanity-checked is not an oracle.
- Docs one day old can already lie: PARAM_STORAGE P1 landed the same morning its doc
  said "not built". §8.9 exists because review-time truth passes cannot keep up with
  landing-time drift.

## What's next

1. UI_AUTOMATION P1–P2 build wave (cleared; Peter launches the Opus orchestrator).
2. PARAM_STORAGE P2 (strong-model session, next in Peter's feature ranking).
3. VERIFICATION_DEBT burn-down: VD-003 (glb held-out fixtures) is runnable today;
   VD-001/VD-002 unlock at L3 when the automation layer lands.
4. Docs-hygiene pass (archive moves + June-doc anchor refresh) — low priority,
   mechanical.
