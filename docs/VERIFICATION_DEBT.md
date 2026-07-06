# Verification Debt — the unverified-surface ledger

<!-- index: Live ledger of shipped-but-not-fully-verified surfaces. One entry per gap between the verification level a landing reached and its target; burned down or consciously carried every wave. Format and rules: DESIGN_DOC_STANDARD.md §10. -->

Why this exists: "unverified interactively" notes used to live in landing reports and
memory, where they decayed silently into "shipped" — the bugs Peter found in the app
on 2026-07-05 (automation lanes present but not visibly working, glb import behaving
hard-coded) were all previously *recorded* as unverified, and nothing acted on the
record. This file makes the debt durable, in-repo, and impossible to close by
forgetting.

Rules (normative home: `DESIGN_DOC_STANDARD.md` §10):

- Every landing appends one entry per gap between the level reached and the target
  level (L0–L4 ladder in §10).
- Every orchestration wave either burns entries down (verify → move to **Closed**
  with date and how) or consciously carries them — the landing report says so.
  Silence is not carrying.
- IDs are stable (`VD-NNN`), never renumbered — they are referenced from landing
  reports (committed under `docs/landings/`, per §8.10), BUG_BACKLOG `Escaped:`
  lines, and memory.

---

## Open

### VD-001 — Automation lanes P1–P4: runtime pointer→command editing path — **L3 reached** / L4 target
Landed 2026-07-04 @ `8b306de0`. **L3 burned down 2026-07-05 (Opus):**
`scripts/ui-flows/drag-automation-point.json` resolves Mirror's middle breakpoint by its
`automation_lanes` surface target and drags it through the real input path (pointer →
viewport events → `InteractionOverlay` → `AppEditingHost` → automation command on the scene
Project); the re-dump confirms the point moved 607.88 → 622.55px, x/beat and the other points
unchanged. The runtime pointer→command→redraw automation-editing core **works headless**.
**Consequence for the in-app bug:** since the core edit path is functional, the "lanes present
but not visibly working" Peter hit is downstream of this core — live-app rendering/visibility or
a live-only wiring the headless overlay path bypasses. **L4 (Peter watching a lane edit take
effect in the running app) remains the target** and is where the real symptom must be triaged.

### VD-002 — Preset library + picker P0–P6: interactive GUI matrix — L2 reached / L3 target — **BLOCKED on driver reach**
Landed 2026-07-04/05 (last `4c860cad`). Drag-drop, search-clear, the management
matrix, and thumbnail display are physically unautomatable headless today.
**Correction 2026-07-05 (Opus): NOT runnable at L3 via the current P2 `--script` driver.**
The earlier "now runnable" note assumed the driver could open and drive the picker; it can't.
Opening the Add-Effect browser / preset picker routes through app-level `PanelAction`s that the
driver's `apply_panel_actions` deliberately does **not** implement (it handles only
`LayerClicked`; the rest need `ui_bridge::dispatch` + `UserPrefs::load()`, breaking headless
determinism). The picker panel lives in `UIRoot` but nothing in the two proving scripts opens
it, and there is no `AutomationAction` to open a popup. So the four picker flows need one of:
(a) a P3 live door into a running app, (b) new scene fixtures that pre-open the picker (single
frame only — no animation, no search text since `AutomationAction::Text` has no headless seam),
or (c) a driver extension that implements the open-picker dispatch headlessly. Until then the
interim Peter click-script (L4) is the only path. Same reach gap blocks the BUG-026 frame-0
repro (see VD-006).

### VD-003 — glTF import: correctness beyond the development fixture — **L2 reached (geometry)** / L2 target; textured + production-drop path still owed
Landed with the glTF wave (foundation @ `47c878d7` + follow-ups). Peter reports
in-app import behavior "hard-coded or buggy" (2026-07-05, exact repro not yet triaged).
**Held-out geometry gate DONE 2026-07-05 (Opus):** both held-out fixtures rendered through
the `render_scene` mesh-snapshot harness (input now overridable via `MESH_SNAP_GLB`, commit
`e7560cb4`) and were **looked at**, not just pixel-counted:
- `lowe.glb` (1.9M verts) → a clean, correctly-formed lion statue on its plinth.
- `cc0__japanese_apricot_prunus_mume.glb` (4.3M verts) → faithful blossom branches, PLUS a
  stray small cube beside the model. Triaged: the cube is a leftover Blender default-cube
  (`Material.001`) baked into that source asset — lowe (a different pipeline) has none — so it
  is a data problem in the asset, **not** an import bug.
Conclusion: glTF **geometry** import is faithful and not hard-coded; the "hard-coded or buggy"
symptom is NOT in the geometry path. **Still owed** (does not gate L2, but keeps this open as a
lens on Peter's report): (a) the *textured* path (this harness applies a default green material,
no albedo — see the separate `gltf_textured_azalea…` proof for the textured route), and (b) the
*production drop-import* UI path (dragging a `.glb` into a layer), which is what Peter actually
exercised. **Fixtures are large** (lowe 43 MB, apricot 85 MB) and remain untracked — committing
127 MB of binaries is Peter's call (recommend git-lfs or keep them as local held-out assets).

### VD-004 — Audio layer export mixdown — L1 reached / L2 target
`audio_mixdown.rs` offline mix is unverified on a real export (recorded in memory as
"unverified on real export" since it shipped). Burn-down: one real export of a
stem-bearing project; listen to / inspect the output file.

### VD-006 — BUG-026 batch-2 popup entrance-tween fix: running-app confirmation — L2 reached / L4 target
Fix landed 2026-07-05 (commit `01c15213`) for the "no popup background until mouseover" bug —
root-caused as a missing animation-poll (see BUG-026). Gate green (clippy; `manifold-ui --lib`
604/604), but the headless `--script` driver has no frame loop and ticks `enter_anim` off
wall-clock, so it **cannot** exercise this timing bug. Burn-down: open the Add Effect browser in
the running app and confirm the dark background panel is present immediately, before moving the
mouse (and that the fade-in reads smoothly). Peter owns this L4 observation.

### VD-007 — PARAM_STORAGE P2 storage swap: GPU value-parity + live-app behavioural confirmation — L2 reached / L3 target
Landed 2026-07-05 (`docs/landings/2026-07-05-param-storage-p2.md`). The id-keyed apply path
(`apply_bindings` resolving by `source_id` instead of a positional index) is covered by 34
`param_binding` unit tests and the gpu-proofs suite COMPILES, but the GPU value-parity suite was
not run (P2 touches no shader/kernel/uniform). The three production behavioural fixes the test-pass
surfaced — revert-prunes-orphaned-user-params, calibration-reaches-the-renderer (D6),
gen-type-undo-restores-exact-arity — are unit/integration-tested but not exercised in a running app.
Burn-down: (a) `cargo test -p manifold-renderer --features gpu-proofs` for the GPU parity run;
(b) the running-app click-script in the landing report. Peter owns the L3 live observation.

### VD-008 — PARAM_STORAGE P3 transport topology guard: running-app confirmation — L2 reached / L4 target
Landed 2026-07-05 (`docs/landings/2026-07-05-param-storage-p3.md`). The transport bridge now stamps
each modulation block with `ParamManifest::topology()` and skips a block on apply when the live
topology no longer matches — closing the same-length-reorder misroute the old `len == len` guard
missed. Covered at unit level by the two `content_state::modulation_topology_guard_tests` (the exact
reorder-skip case + a control), but the *live* behaviour — a modulation display staying on the
correct slider when a neighbour param is deleted mid-modulation — is not exercised in a running app
(headless tests have no modulation loop / live UI). Burn-down: the running-app click-script in the
landing report (LFO on a slider, delete a neighbour, confirm the display stays put). Peter owns the
L4 observation. This is the one P3 gate step headless tooling cannot reach.

### VD-009 — PARAM_STORAGE P4 Ableton/OSC by-id resolution: real-hardware round-trip — L2 reached / L3 target
Landed 2026-07-05 (`docs/landings/2026-07-05-param-storage-p4.md`). Both live-hardware input paths
now resolve param mappings by manifest id against the live manifest, so user-added / glb-imported
params are mappable (Ableton) and addressable (OSC) instead of being silently dropped. Unit-proven:
the two repros, dispatch-by-id, and a guard that bundled OSC addresses are byte-identical to the old
positional derivation. Not exercised with real hardware. Burn-down: (a) map an Ableton macro to a
user-added / glb-generator param in the running app and confirm it moves; (b) send OSC to
`/master/{prefix}/{user_param_id}` and confirm the param moves, and that a bundled param's existing
address still lands byte-for-byte. Peter owns the L3 live observation.

### VD-010 — PARAM_STORAGE P5 inspector single-source: angle-card degree readout in a running app — L2 reached / L4 target
Landed 2026-07-05 (`docs/landings/2026-07-05-param-storage-p5-inspector.md`). `is_angle` now has a
single home on the manifest spec; an exposed angle param's card is proven to carry the flag through
the manifest + synth + JSON round-trip by unit test. Not observed rendering. Burn-down: in the graph
editor expose an inner `ParamType::Angle` param (or load a glTF and open its camera-orbit/tilt/FOV
card), confirm the card slider reads out `NN°` (not radians), and that a text edit round-trips
degrees↔radians without drift. Peter owns the L4 live observation.

### VD-011 — AUDIO_SENDS_UX P1 per-send gating: trace-count run with real audio — L1 reached / L2 target
Landed 2026-07-06 (`docs/landings/2026-07-06-audio-sends-ux.md`). The consumed-set walk is
unit-proven (4 tests on `Project::analysis_consumed_sends`) and the per-send skip is in the tick
path, but the doc's own P1 gate — `MANIFOLD_AUDIO_TRACE=1`, 16 sends, one bound param, log shows
"analyzed 1 send(s)" (2 with the scope open) — needs a running app with a capture device. The
instrument is shipped and env-gated. Burn-down: Peter (or a future L3 flow) runs the trace launch
per the landing report's click-script step 5. Peter owns the L2 observation.

### VD-012 — AUDIO_SENDS_UX P3 calibration drags: live feel + undo-step + no-capture-restart — L1/L2 reached / L4 target
Landed 2026-07-06 (same landing report). Drag arm/commit sequences and the dB/fraction math are
unit-proven (3 on_event tests); the layout and non-dim anchoring are PNG-verified (L2). Not
observed: meter following a gain drag against live audio, absence of capture-restart glitch, and
exactly one undo step per drag gesture — all inherently running-app. Burn-down: click-script step 4.
Peter owns the L4 feel-pass; it is the acceptance gate for the panel per the wave brief.

*(VD-001–004 seeded 2026-07-05 from the memory corpus plus Peter's in-app findings; VD-006 added
2026-07-05, VD-007 at P2 landing, VD-008 at P3 landing, VD-009 at P4 landing, VD-010 at P5-inspector
landing. VD-005 closed at P2 landing. The full backfill pass over recent landings is still owed and
will extend this list.)*

## Closed

### VD-005 — UI_AUTOMATION P1 selector surface: no scripted drive — CLOSED 2026-07-05 (L3 reached)
Opened at P1 landing (`3294eb9d`, L2). **Closed by P2 landing** — the `drag-clip.json`
flow resolves a `timeline_clips` surface target and drives it through the real input
path (clip moved 230→314px), and `select-and-inspect.json` resolves a widget by
name/text and clicks it: the selector surface is now scripted-driven end-to-end (L3).
Residual carried as an organic-growth item, NOT debt: the `editor` scene still surfaces
zero *named* widgets (graph-editor chrome unnamed headless) — name points as flows need
them, per §3 ("coverage grows organically"). Landing report:
`docs/landings/2026-07-05-ui-automation-p2.md`.

*(none yet)*
