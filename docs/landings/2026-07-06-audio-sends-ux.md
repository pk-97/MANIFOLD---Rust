# AUDIO_SENDS_UX P1–P4 — landed 2026-07-06 @ 85684018

**Branch:** `feat/audio-sends-ux` · **Level reached:** L2 (P2/P3/P4 PNG-verified by orchestrator; P1 L1 + shipped trace instrument) / targets: L2 per doc, L4 residue queued (§10)
**Doc status line (quoted verbatim):** "**Status:** SHIPPED P1–P4 2026-07-06 (orchestrated wave, Fable + Sonnet workers, branch `feat/audio-sends-ux`; landing report `docs/landings/2026-07-06-audio-sends-ux.md`) · **P5 DROPPED by Peter 2026-07-06 (\"I don't want them. Not useful\") — do not build, do not re-propose** · L4 residue: Peter's in-app pass owed (P1 trace-count run, P3 drag feel + undo-step check) · BUG-047 logged (panel overflow past SCOPE_H_MIN floor, LOW) · approved by Peter 2026-07-04 · Fable · D5 word confirmed: \"Source\" · **baseline-reviewed 2026-07-05, cleared**"

Wave shape: Fable (high) orchestrating, one Sonnet worker per phase, sequential in one worktree.
Phase commits: P1 `9ba1f6e1` · P2 `bc7a63fc` + `ebd43428` (orchestrator-directed legibility fixes:
consumer-row jump chevron, per-send section headers) · P3 `6fe55ff9` + `f2c58bdb` (orchestrator-caught
clipping defect: selection normalized after sizing; root-caused and regression-tested) · P4 `ace7f0ee`.
P5 cancelled by Peter before build.

## What shipped (instrument terms)

- **P1 — analysis is pay-per-use.** A send (now "source") is analyzed only if something listens:
  ≥1 enabled audio mod, ≥1 enabled trigger route, or the scope tap. One bound param no longer costs
  all 16 sends ~1ms/tick on the content thread; unbound sends cost zero. Consumed set rebuilds only
  on project change (existing DataVersion gate); per-tick cost is one hash lookup per send.
- **P2 — the whole star in one place.** Selecting a source shows Inputs (capture channels + feeding
  layers, editable both ways via the same command the layer header fires) and Consumers (every bound
  param + trigger route, click jumps to the owning layer). The on-stage "why isn't this visual
  moving" debugging surface.
- **P3 — calibrate against the live show.** The panel stops dimming and docks right (38%, full
  height, Modeless, outside clicks pass through — deliberate: no accidental dismissal mid-set).
  Gain and trigger-sensitivity value labels are horizontal drag zones (1px = 0.1dB / 0.5%), live
  via MutateProjectLive, one undo step per gesture.
- **P4 — "Send" → "Source"** in user-facing strings only ("+ Add Source", "No source"); types,
  serde, commands untouched (manifold-io tests prove the save format).

## Gate results (verbatim tails)

Per-phase (workers, in worktree): clippy --workspace -D warnings clean at every phase;
manifold-ui --lib 616→620 passed (4 P2 + 3 P3 + 1 P3-regression tests added); manifold-core --lib
311 passed (4 new consumed-set + 2 consumer-label tests); manifold-audio --lib 54 passed;
manifold-io --lib 37 passed. manifold-app has no lib target (confirmed, no scaffolding invented).
Landing gate (orchestrator, worktree, post-merge of origin/main): see terminal log this session —
clippy --workspace -D warnings clean; cargo test --workspace green; audiosends PNG regenerated and
read after merge.

## Deviations from brief

- View-model carries `LayerId` (not the doc sketch's `usize`) — matches the real
  `SetLayerAudioSend`/`LayerClicked` action types; reported by worker, accepted.
- PanelAction drag variants named `AudioSendGain*`/`AudioSendSensitivity*` (not the suggested
  `AudioGain*`) — avoids collision with the existing layer-keyed `AudioGain*` actions; accepted.
- P1's pure consumed-set fn lives in `manifold-core/src/project.rs` beside `sends_with_pitch_mods`
  (existing precedent), not inside `audio_mod_runtime.rs`; accepted.
- Orchestrator UX overrides beyond the doc: consumer-row "›" affordance; "Inputs — X"/"Consumers — X"
  header suffixes; outside-click = pass-through (doc left "pass through or close" to precedent).
- P5 + D8 cancelled by Peter mid-wave ("I don't want them. Not useful") — recorded in doc.

## Shortcuts confessed (rolled up from phase reports)

- P1: none on code. AUDIO_INFRASTRUCTURE §7 staleness flagged, not fixed by the worker (out of
  phase scope) — **fixed at landing** (this session: §7 modality claim superseded per D6,
  `FeatureFrame.amplitude` references corrected, both phase-list mentions).
- P2/P3/P4: none. P4 process note: `ui-snap` writes PNGs cwd-relative — a worktree run needs its
  cwd IN the worktree, `--manifest-path` alone silently writes into the main checkout's target/.

## Verification debt

- **VD-011 opened** — P1 trace-count run with real audio (L1 reached / L2 target).
- **VD-012 opened** — P3 drag feel + one-undo-step + no-capture-restart (L1/L2 reached / L4 target).
- **BUG-047 logged** — panel sections can still clip past SCOPE_H_MIN floor on a source with ~18+
  combined input/consumer rows (LOW; fix shape is a deliberate UX call — cap+"+N more" or
  ScrollContainer — not improvised here).
- Carried, unchanged: none from this wave's scope.

## Click-script for Peter (≤2 minutes)

1. Cmd+Shift+A — expect: Audio Setup docks to the RIGHT edge (~38%), show fully visible and
   undimmed behind it; clicking the timeline/preview does NOT close the panel; Escape does.
2. Click a source's swatch — expect: "Inputs — X", "Spectrogram — X", "Triggers — X",
   "Consumers — X" sections, all for that source.
3. Click a consumer row (each has a "›" at the right edge) — expect: the owning layer selects in
   the timeline; the panel stays open.
4. With audio playing, press-drag horizontally on the gain "0 dB" value label — expect: value and
   meter follow live with no audio glitch; ONE Cmd+Z reverts the whole drag. Same on a trigger
   sensitivity "50%" label.
5. Launch with `MANIFOLD_AUDIO_TRACE=1`, 16 sources configured, one param bound — expect terminal:
   `[AudioMod] analyzed 1 send(s): [...]`; open the scope on a second source → `analyzed 2 send(s)`.
6. Vocabulary sweep: panel shows "+ Add Source"; an audio layer's header chip shows "No source";
   nothing user-visible says "Send".
