# Landing: kick scope lane + ScopeColumn typed overlay layout

**Landed:** 2026-07-07 @ `b6aed008` (`wave/kick-overlay`, retired) + GPU-proof follow-up (`wave/scope-gpu-proof`).
**Brief:** Opus, scratchpad (`kick-overlay-brief.md`, session 45a4aade) — fix the overlay-layout duplication first, add the kick marker second, so the marker proves the fix.

## What shipped

**P1 — the layout is a type.** The scope overlay (4 centroid traces + onset
tick lanes per spectrogram column) was a packed float strip whose stride (7)
and field order were hand-synced across six sites (analyzer, mod runtime,
content thread/state, app_render, WGSL, mod_harness's CPU port). Now:
`ScopeColumn`/`ScopeOnsets` in `manifold-spectral/src/scope.rs` (repr(C),
all-f32, const-asserted padding-free) is the one definition; every Rust site
speaks the type; the shader receives stride/onset base/count + lane colours
via its `Params` uniform and draws lanes in a loop. Upgrades over the brief
(accepted by Peter): type instead of shared constants (kills the push-order
convention and the need for a runtime assert), lane colours in the uniform
(P2 touches one crate), naga test asserting Rust/WGSL `Params` sizes match.

**P2 — kick lane.** `ScopeOnsets.kick` (bottom lane, below Low/Mid/High which
shift up one) + magenta `(1.0, 0.0, 0.8)` — Peter rejected the brief's amber:
the jet colourmap paints yellow/red/white exactly where loud low-end lands,
so amber dissolves; magenta is the one hue family the ramp never produces.
Producer: `fired(b[1].kick)` at the analyzer scope push.

**Fixed in passing:** `app_render` overflow drain used a stale `cols * 4`
stride (latent overlay desync under scalar overflow) — now structurally
unwritable (`Vec<ScopeColumn>`, excess is just `cols`).

## Verification

- P1 behaviour-preserving: `mod_harness` render of `bad_guy_128bpm/mix.wav`
  **byte-identical** before/after.
- P2 value level: `streaming_analyzer_scope_reports_kick_fires` — two
  synthetic 120→45 Hz kicks fire the lane exactly twice, end to end.
- P2 visual: exactly **42 magenta ticks** in the bottom lane on the
  `bad_guy_128bpm` render — the fire count previously verified for that
  fixture.
- Metal path: `spectrogram::gpu_tests::onset_lanes_draw_at_their_slots_in_their_colors`
  (new `gpu-proofs` feature on manifold-spectral, off by default) renders the
  real GPU scope and reads pixels back — every lane at its slot in its
  colour, no bleed, unfired columns dark.

## For the instrument

The Audio Setup scope is now the kick-tuning monitor for P3: play a track,
watch the magenta ticks against the low-end, judge the detector's hit rate
live. Adding the next overlay scalar (snare? hat?) is a field + colour in
`scope.rs` plus one analyzer push line — nothing else, enforced by the
compiler.

## Residue

- Peter's P3 feel-pass (bind Kick, tune on real sets) — unchanged, owed.
- `wave/kick-overlay` deleted after `merge-base --is-ancestor` confirmation;
  remote branch kept for record.
