# Cross-Design Coherence Audit — running ledger

**Status: IN PROGRESS · 2026-07-10 · Fable (fresh session, per the approved brief)**
**Scope: every board entry APPROVED-not-built or IN PROGRESS as of 2026-07-10 (board regenerated this session). 36 designs.**

Method (per brief): build-order completeness check → cluster by seams → per-design ledger
(surfaces claimed · amends/assumes · prerequisites · sampled code anchors) → per-cluster
collision hunt → kill-pass each finding against the second doc's actual text.

This file is the survival mechanism across context summarization — it is committed
incrementally and each section is self-contained. Final outputs (conflict list, per-design
verdicts, build-order update, Opus-vs-mechanical handoff split) accrete at the top once
clusters complete.

---

## 0. Build-order completeness check (brief step 1)

`DESIGN_BUILD_ORDER.md` §1 claims to cover "every APPROVED-not-built design doc." Checked
against the regenerated board 2026-07-10:

**Missing entirely (approved 2026-07-08/09, never added):**
- VIDEO_IO (approved 2026-07-09)
- UI_HARNESS_UNIFICATION (approved 2026-07-09)
- PERF_BUDGET_GATE (approved 2026-07-09)
- LIVE_RECORDING_PROOFS (approved 2026-07-09, release-gating per STRUCTURAL_AUDIT_VERDICTS)
- BOX3D_PHYSICS (approved 2026-07-09)
- AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION (approved 2026-07-09)
- RENDER_SCENE_UNBOUNDED_LIGHTS (approved 2026-07-06)
- AUDIO_OBJECT_TRACKING (approved 2026-07-06)
- AUDIO_OBJECT_INGEST (approved direction 2026-07-06)
- KICK_SWEEP_EVENT (IN PROGRESS — remaining phases unsequenced)
- (directions, arguably pre-queue: MAPPING_GRAMMAR, AUTO_POPULATE — noted, not counted as findings)

**Present but shipped (should be removed per §5 maintenance):**
- AUTOMATION_LANES (P1–P4 shipped; P5 partial — check whether a row remnant is warranted)
- MATERIAL_SYSTEM (M1–M6 shipped — row already annotated but still present)
- OVERLAY_SESSIONS_AND_PICKER (P1–P2 shipped)
- PRESET_LIBRARY (P0–P6 shipped)
- TIMELINE_INGEST (P3+P4+P5 shipped; P1/P2 parked on BUG-028 — remnant row may be warranted)
- PARAM_STORAGE_BOUNDARIES (P1–P3 shipped 2026-07-09)
- PARAM_STORAGE row says "P1–P5 SHIPPED" but board says IN PROGRESS — reconcile with doc.

**Stale annotations spotted (verify before fixing):**
- §3 item 4: "MULTI_DISPLAY P2 ⏸ BLOCKED pending §6.1 seam-hardening" — board says §6.1a
  resolved 2026-07-06. Check DESIGN_HARDENING_QUEUE item 2.
- §3 item 6: "MEDIA_BACKEND P1 ⏸ PARKED (§3 addendum needed)" — board says §3a addendum
  resolved 2026-07-06. Check DESIGN_HARDENING_QUEUE item 1.

## 1. Cluster assignment (initial; refined by reading seams, not titles)

- **3D + render_scene:** REALTIME_3D · SCENE_BUILD_AND_GROUP_PARAMS · RENDER_SCENE_UNBOUNDED_LIGHTS · SIMULATIONS · BOX3D_PHYSICS · GAUSSIAN_SPLATS · IMPORT · ML_NODES
- **Perform path:** SESSION_MODE · PERFORM_SURFACE · MULTI_DISPLAY · GIG_RESILIENCE · APP_SHELL · DJ_PERFORMANCE · PRO_DJ_LINK · ABLETON_SHOW_SYNC
- **Audio:** KICK_SWEEP_EVENT · AUDIO_ANALYSIS_ACCURACY · AUDIO_OBJECT_TRACKING · AUDIO_OBJECT_INGEST · AUDIO_SETUP_DOCK · MAPPING_GRAMMAR · AUTO_POPULATE
- **Param/card:** PARAM_STORAGE · COMPONENT_LIBRARY · MCP_INTERFACE (+ SCENE_BUILD P3 seam)
- **io/export/media:** VIDEO_IO · MEDIA_BACKEND · COMMERCIALIZATION · LIVE_RECORDING_PROOFS
- **LED/output:** LED_STRIPS · PROJECTION_MAPPING (+ MULTI_DISPLAY seam)
- **Dev infra:** UI_AUTOMATION · UI_HARNESS_UNIFICATION · PERF_BUDGET_GATE · VULKAN_BACKEND

(Designs appearing in two clusters get audited in both — cluster membership is a lens,
not a partition.)

---

## Per-design ledger

(filled per cluster; format: surfaces claimed · amends/assumes · prereqs · anchors sampled)

---

## Conflict list

(accretes; each entry: severity · the two docs · one-line collision · which doc must change)

---

## Per-design verdicts

(clean / needs amendment (named) / stale anchors (named))

---

## Handoff split

(Opus amendment session vs mechanical fixes)
