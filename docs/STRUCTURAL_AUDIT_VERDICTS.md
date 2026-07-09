# Structural Audit — Verdicts

Authored 2026-07-09 by Fable, from [BUG_CORPUS_DOSSIER.md](BUG_CORPUS_DOSSIER.md) (Sonnet miner:
all 82 backlog bugs classified, 1672 fix commits density-mapped, cross-referenced against
FOUNDATIONAL_GAPS.md A1–A7/Part B and CORE_ENGINE_FINDINGS.md F1–F17). Purpose: after Fable
availability ends, Opus designs and Sonnet builds against THESE verdicts instead of rediscovering
them. Status: verdicts final unless Peter overrides.

## §0 Evidence status — read before trusting

Verdicts rest on the dossier plus direct verification of the three load-bearing claims:
BUG-006/007/008 entries read in full (BUG_BACKLOG.md:953–1016), BUG-060's REOPENED status and
reopen mechanism read (:1309–1322), F5's exact text read (CORE_ENGINE_FINDINGS.md:157–163).
The per-bug table in dossier §2 was NOT re-audited row by row — before acting on any single row,
read that bug's backlog entry. Scope: verdicts are drawn from the backlog-covered period
(post 2026-06-23); pre-backlog git history (dossier §4) is admitted as corroborating evidence
only. Known corpus gap: March–May (vsync saga, TexturePool abandon-and-retry, wgpu migration)
has zero backlog coverage — verdicts undercount manifold-app/manifold-gpu historical churn.

## §1 Headline

The codebase is not riddled with wrong designs; it is riddled with **unenforced right designs**.
Majority class: missing-invariant-enforcement (20) + stale-state/projection (8) +
identity-minting (3) = 31/82 bugs where the invariant existed in prose (map doc, CLAUDE.md rule,
memory) and nothing in code enforced it. The corpus's own strongest datapoint (backlog, "The
pattern behind all of this"): the one duplication path with a test never regressed; every path
without one did. Two genuine structural faults stand out (UI↔content projection seam; engine
mutation authority), plus one severe blocked feature (BUG-053, HDR recording).

Consequence for how Opus/Sonnet work from here: **"fixed" for any bug in a recurring class means
fix + the enforcement that makes recurrence impossible + backlog entry updated.** The two proven
templates to copy: manifold-io's guard+test pattern (forward-version guard, fsync negative-gates —
1/6 still open, best ratio of any early-corpus crate) and manifold-audio's accuracy-gate pattern
(every fix lands with a scored selftest gate — 8 bugs, all one-offs, none recurred).

## §2 Verdicts by subsystem

| Subsystem | Verdict | Fix owner |
|---|---|---|
| manifold-ui ↔ content projection seam | **structurally-wrong** (= A1, confirmed) | Opus design |
| manifold-playback | sound-but-underspecified; ONE structural fault: engine mutation authority (F5) | Opus design |
| freeze/fusion compiler | sound design, **unenforced contract** (miner's "structurally-wrong" downgraded — see below) | Sonnet fixes + Opus verifier design |
| manifold-media/recording | structurally-wrong, severe, n=1 (BUG-053 blocks HDR recording) | LIVE_RECORDING_PROOFS design already PROPOSED |
| manifold-core | sound-but-underspecified; BUG-080 confirmed as its own design item | Opus design (already self-nominated) |
| manifold-app | sound-but-underspecified; A7 feature-matrix rot = 6 of its bugs, one CI gate kills the class | Sonnet, cheap, do first |
| manifold-io | closest to target state; export its pattern | BUG-063 remainder only |
| manifold-audio | sound; export its pattern | — |
| manifold-editing, manifold-gpu, docs tooling | sound at current n | — |
| identity-on-duplicate (A5, cross-crate) | pattern exists + tested in graph-node paste; port it | Sonnet (see §4 Q6) |
| BUG-069 licensing | excluded — legal/dependency risk, not architecture; tracked in `audio-analysis-accuracy` memory / opus-prompt-pack Prompt 14 | — |

### UI↔content projection (verdict: structurally-wrong; priority #1 for the release)

Corpus and FOUNDATIONAL_GAPS A1 agree independently: no enforcement layer for UI/content snapshot
sync, every field hand-threaded (bugs 015, 026, 060, 076; 060 REOPENED). The reopen mechanism is
the diagnosis in miniature: the P1 fix was verified through the headless harness's
`UITree::traverse()` path while the live app renders via `panel_cache_info()` — two render paths,
the harness proved the wrong one (BUG_BACKLOG.md:1320–1322). UI_HARNESS_UNIFICATION (approved,
Sonnet-executing) closes the *verification* half. The *construction* half is the A1 design: an
enforced projection layer so a new screen cannot hand-thread a field wrongly and compile. Every
upcoming screen multiplies this surface — this design gates the release work, author it first.

### manifold-playback (verdict: sound-but-underspecified; the fault line is mutation authority)

Not wholesale structurally wrong — CORE_ENGINE_MAP shows a deliberate, comprehensible design.
The genuine structural fault: per-frame engine writes to serialized settings have no sanctioned
path, so they bypass EditingService ad hoc — F5 verbatim: `tick_sync_controllers` overwrites
`project.settings.clock_authority` (a manual authority choice is reverted one frame later) and
`sync_project_bpm_from_current_beat` writes `project.settings.bpm`, both outside the gateway,
marked DECISION NEEDED. Answer to dossier Q3 (bandwidth vs structural resistance): mostly
bandwidth, but the F5-class items specifically stalled because they need a *decision* nobody
owned, not code. Fix shape — Opus authors **ENGINE_STATE_AUTHORITY**: classify every
engine-written field as engine-owned (sanctioned fast path, own versioning, explicitly no-undo)
vs user-owned (gateway only), then triage F1–F17 into needs-this-design vs plain-fix. Stage
stakes are the highest in the codebase: this is the timing crate, and a timing bug becomes the show.

### Freeze/fusion compiler (verdict: sound design, unenforced contract — miner downgraded)

The miner suggested structurally-wrong; I'm overruling with evidence: all 8 campaign bugs have
precise, LOCAL root causes with cheap fix shapes written in the backlog — BUG-007 is one line
(`configured_construct` for the one bare-construct hold-out), BUG-008 has a conservative
fail-closed option (refuse >1 array external at `build_region`). Wrong designs don't localize
like that. What the campaign actually proved: FREEZE_COMPILER_MAP's documented invariants
(cut rules, marker ABI, precision contract) are enforced nowhere at fuse time. Two actions:
(1) Sonnet wave lands BUG-006–012/014 off the backlog's own fix shapes, each gpu-proofs-gated;
(2) Opus authors **FREEZE_VERIFIER** (= new FOUNDATIONAL_GAPS cluster A8): fusion-time invariant
checks — state-statelessness, array-length agreement, single-entry-point, content-key hash
stability — that fail closed to unfused execution. A fused kernel that silently diverges from
the editor is a stage-facing correctness bug wearing a perf-feature costume.

### Standing hazard zone (not a verdict — a rule)

The vsync/present neighborhood (`app.rs`/`app_render.rs`/`content_pipeline.rs`) produced 4+
distinct root causes under one symptom in two weeks (pre-backlog, dossier §4) despite standing
memory warnings. Warnings don't enforce. Rule for executors: any change in that neighborhood
requires reading `docs/VSYNC_AND_FRAME_PACING.md` first and a soak run before landing.

## §3 Execution queue (in order)

Opus design windows:
1. **A1 UI projection layer** — release-critical, gates every new screen. Method: DESIGN_AUTHORING.md; verification half already covered by UI_HARNESS_UNIFICATION.
2. **ENGINE_STATE_AUTHORITY** — resolves F5 and triages F1–F17.
3. **FREEZE_VERIFIER (A8)** — after or parallel to the Sonnet fix wave.
4. **BUG-080 param-manifest construction** — already self-nominated for an Opus pass.

Sonnet waves (no design needed, briefs can cite this doc + the backlog fix shapes):
- A7 feature-matrix CI gate (one job compiling the feature matrix; kills a 6-bug class).
- Freeze campaign fixes BUG-006–012/014, gpu-proofs-gated per fix.
- A5 identity ports: first a direct code comparison (does `EffectId`/`ClipId` duplication lack the
  trait/macro shape `NodeId` has? — dossier Q6), then port the graph-node paste pattern + one
  regression test per duplication path.
- BUG-053 execution once LIVE_RECORDING_PROOFS is approved — it is on the export path, which is
  the release's second pillar; treat as release-gating, not n=1 trivia.

## §4 Dossier §6 questions — answers

1. A8 for freeze/fusion: **yes** — anchor it to FREEZE_COMPILER_MAP.md; fold into
   FOUNDATIONAL_GAPS.md at landing time (deliberately not edited on this branch to keep the diff
   one file).
2. BUG-080 vs F5 same shape? They rhyme ("invariant suspended during a privileged window") but the
   fixes differ — construction-time completeness vs runtime write authority. Two designs, shared lens.
3. Answered in §2 playback verdict: bandwidth + unowned decision, not structural resistance.
4. Pre-backlog history: corroborating evidence only; scoped out of verdicts, recorded in §0 as a
   known undercount.
5. BUG-069: excluded from architecture verdicts (see §2 table).
6. Not assumed copy-paste; comparison step is first in the A5 Sonnet brief (§3).
