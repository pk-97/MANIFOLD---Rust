# AUDIO_SETUP_DOCK Wave 2, batch 2 (P7+P8) — landed 2026-07-11 @ <merge SHA, filled at push>

**Branch:** `wave/audio-dock-wave2` · **Level reached:** L1 (unit tests) for P7's tap-follow logic and P8's row-set changes; L1 with a real GPU readback proof for P7's band-dim shader math. **P7's L3 (scripted flow) and L2/L4 (live full-pipeline PNG / rig look) are NOT claimed** — VD-026 carries both gaps.
**Doc status line (quoted verbatim):** `**Status:** **SHIPPED — WAVE 2 COMPLETE 2026-07-11 (P5–P8 all landed)**`

## What shipped

### P7 — Scope follows the trigger drawer

**Tap-follow.** Expanding any fire-mode drawer (a clip trigger, or a param card's
`is_trigger_gate` row) re-taps the Audio Setup scope to that config's send; collapsing
falls straight back to the panel's own selected send. New accessors
`open_fire_mode_drawer_send`/`open_fire_mode_drawer_band` on `ParamCardPanel`,
`AudioTriggerSection`, and `InspectorCompositePanel` (filtered to fire-mode configs
only), surfaced through `UIRoot`. `app_render.rs`'s existing "push the scope's selected
send" block tries `open_fire_mode_drawer_send()` first, falling back to the panel's own
`selected_send()` — recomputed fresh every frame, so "session-only, never persisted"
falls out for free with no separate restore path.

**Band dimming — as-built correction from the brief.** The brief's own wording ("two
translucent overlay quads") was read literally at first, but reading `ui_frame.rs`'s
render order found the VQT waterfall blit is the LAST GPU draw call every frame — after
the panel atlas and the overlay flush. A UI-tree quad positioned inside `scope_rect`
would be painted over by that blit on every live-data frame; the existing per-band level
meters are deliberately kept OUTSIDE `scope_rect` for exactly this documented reason
("the blit would otherwise cover them"). The dim is instead computed in the SAME shader
pass that already draws the crossover divider lines (`spectrogram.wgsl`): `Params`
gained `dim_lo_y`/`dim_hi_y` (the KEPT y-range, same bottom-up convention as
`band_lo_y`/`band_hi_y`; `< 0` disables), `Spectrogram::render` gained a
`dim_range: Option<(f32, f32)>` parameter, and the fragment shader darkens (`× 0.28`)
the colour-mapped magnitude outside the kept range — applied BEFORE the
divider/centroid/onset/cursor draws, so those stay full-brightness (fire feedback lives
in the drawer meter, §5 item 6, never re-opened). `VqtPassState` gained `band_dim`,
resolved once per frame from `UIRoot::open_fire_mode_drawer_band()`.

### P8 — Panel de-clutter

- **(a) Kick lane removed** from the scope's onset-tick display: `ScopeOnsets` drops the
  `kick` field outright (`COUNT` 4→3, `ScopeColumn::STRIDE` 8→7). Every consumer
  (`LANE_COLORS`/`LANE_LABELS`/`lanes()`, the WGSL uniform, the `mod_harness` CPU port)
  is already generic over the count, so the removal cascaded with a one-line push-site
  edit (`analysis.rs`) and one obsolete end-to-end test deleted
  (`streaming_analyzer_scope_reports_kick_fires`). The ridge-only kick detector and the
  drawer's Kick feature button are completely untouched.
- **(b) Cap chip removed** from the send row along with its click-to-reveal routings
  popup (`AudioSendRoutingsClicked`, `send_routings()`, `DropdownContext::
  AudioSendRoutings`) — the same content (device + feeding layers) now lives
  permanently in the read-only Inputs section from (c). `source_label`/`layer_fed`
  dropped from `AudioSendRow` (unconsumed after); `feeding_layers` kept (real project
  data, not row chrome — only its one caller, `feeding_layer_ids`, is gone).
- **(c) Inputs section authoring removed** ("+ Layer", per-layer ×,
  `AudioSendAddLayerClicked`, and the `audio_layers` cache that fed it): the section
  reads `AudioSendRow::routings` directly — device line + one line per feeding layer,
  plain text. The "(missing layer)" repair copy now points at the layer header's Send
  dropdown, the one surviving authoring path (same `SetLayerAudioSend` command, one
  owner).
- **(d) St/Mo toggle deleted** (`AudioSendStereoToggle` + `RowControl::Stereo` + the
  button). The channel dropdown now enumerates stereo pairs AND single channels
  directly ("Left + Right", "Left", "Right", "Ch 3+4", "Ch 3", "Ch 4"…) via a shared
  `push_channel_pair_rows` helper — `AudioSetSendChannels` already carried any channel
  vec, no model change.
- **(e) Consumers clip-trigger rows reworded** "Clip trigger • Layer • Band" (was the
  Triggers-matrix-era arrow style "Band → Layer"), matching the mod rows' "Layer •
  Effect • Param" bullet convention.
- **(f) `AUDIO_SENDS_UX_DESIGN.md` D2/D3 as-built notes** added: panel-side layer
  routing authoring is superseded, Consumers row format updated.

## Gate results (verbatim)

```
manifold-ui --lib:                        667 passed
manifold-spectral --features gpu-proofs:  8 passed (incl. dim_range_darkens_outside_the_kept_band_only)
manifold-audio --lib:                     57 passed, 1 ignored
manifold-app (3 test bins):                162 + 10 + 1 passed
manifold-core --lib:                       359 passed
cargo build --workspace:                   clean
cargo nextest run --workspace:             3022 tests run: 3022 passed, 8 skipped
cargo clippy --workspace -- -D warnings:   clean
cargo deny check bans:                     bans ok
```
One flaky failure encountered and NOT part of this landing's surface:
`manifold-core::params::tests::bench_resolve` (a machine-load-sensitive timing
benchmark on `ParamManifest::get`, untouched by any P5–P8 change) failed once under
load from this session's own heavy build/test/GPU activity (314–361 ns/op vs a 271.5 ns
ceiling), then passed clean (201.91 ns/op) on rerun after a brief pause. Not a
regression from this wave — no file it depends on was touched.

## Deviations from brief

- **P7 band dimming mechanism** — the brief said "two translucent overlay quads";
  shipped as a shader-side darken pass instead (see above). Mechanism deviation, not a
  behavior deviation — the observable result (up to two dimmed regions, or none for
  Full) matches the brief's intent exactly.
- **P8 entry-state anchor was imprecise** — the brief pointed at
  `manifold-renderer --lib` for `ScopeOnsets`; it actually lives in `manifold-spectral`.
  Caught at execution time (§5's re-verification rule) and the test-scope command
  followed the real crate.

## Shortcuts confessed

- P7: no L3 ui-flow (no harness interact verb for arming/expanding an audio-mod
  drawer yet) and no live full-pipeline PNG of the dim (VQT needs real audio,
  unavailable headless) — both carried as VD-026.
- P8: no new unit test added for `AudioTriggerSection`'s side of the changes (it has no
  test module at all — VD-024, pre-existing, not opened by this wave). Coverage for
  that side rests on compile-time correctness (the deleted fields/methods simply don't
  exist to call) plus the full `manifold-app` test suite passing unchanged.

## Verification debt

- **VD-026 opened** (P7): tap-follow + band dimming has no L3 flow and no live
  full-pipeline PNG. Full entry in `docs/VERIFICATION_DEBT.md`; shares a soundcheck
  session with VD-025's burn-down.
- VD-024 and VD-025 carried unchanged (not touched by this batch).
- No new VD opened by P8.

## Click-script for Peter (≤2 minutes)

1. Open the Audio Setup dock. Note which send is selected/scoped, and that the scope's
   legend now shows only Low/Mid/High — no Kick tick lane.
2. Select a layer and open its Clip Trigger drawer on a DIFFERENT send than the one
   selected in step 1. **Expect:** the scope immediately re-taps to that send.
3. Set the drawer's Band to Low, then Mid, then High, then back to Full. **Expect:**
   the scope dims everything outside the selected band each time, no dim at Full.
4. Collapse the drawer. **Expect:** the scope falls back to the panel's own selection
   from step 1.
5. Look at any send row. **Expect:** no "Cap"/"Mo"/"St" chip — just the label, the
   channel dropdown (try opening it: it now lists "Left + Right" style pairs AND
   individual channels in one list), and the gain/delete controls.
6. Open the Inputs section for a send with a feeding layer. **Expect:** plain text
   ("Layer • <name>"), no "×" or "+ Layer" button. Route/unroute that layer from the
   LAYER HEADER's own Send dropdown instead — confirm it still works and the Inputs
   section updates to match.
7. Look at the Consumers list for a send with an active clip trigger. **Expect:** the
   row reads "Clip trigger • <Layer> • <Band>".
