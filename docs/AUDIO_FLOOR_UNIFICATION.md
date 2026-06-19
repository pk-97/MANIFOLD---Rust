# Audio Floor Unification — one spectrogram, one floor, one truth

The Audio Setup spectrogram must be the single source of truth: whatever the user
sees on the scope is exactly the audio every algorithm runs on — triggers, slider
modulation, the level meters. Before this work that was not true. The detector
fired on bands that read as pure black on screen (a clean kick-only signal threw a
cluster of onset ticks per kick, and the high band fired ticks while its display
region was empty). This doc captures why, and the unified design that fixes it.

## The bug: three floors, not one

Detection did **not** run on the pixels the user sees. There were three
independent level references:

1. **Colour-ramp `db_min` / `db_max`** (`-59` / `0` dB, `SpectrogramConfig`) — the
   display's black point and contrast range. Lives in the shader.
2. **The squelch floor** (`AudioSend.floor_db`, the "Floor" stepper) — a *separate,
   optional* pass that zeroed bins below it. Default off. Compared against the
   **untilted raw** magnitude (`vqt_raw`), while the display and detector both work
   in the **tilted** domain — so the line the user set was not the line they saw.
3. **`ONSET_AMP_GATE`** (`0.12` in the `-59..0` normalized space ≈ `-52` dB) — the
   detector's *private* "loud enough to fire" gate. Invisible, unset by the user.

When a band goes quiet the adaptive flux threshold collapses to `SUPERFLUX_DELTA`
(`1e-3`, ~zero), so the **only** guard between faint, sub-visible energy and a fire
was `ONSET_AMP_GATE` — and it sat at a different level than the screen's black
point. "Black on screen" and "silent to the detector" were two different lines.
Energy living between them fired ticks with no visible cause.

There was also a latent hazard: the pink tilt (`+3 dB/oct`, boosts highs ~`+16` dB)
was defined as **two** constants — `SCOPE_TILT_DB_PER_OCT` (detector) and
`PINK_SLOPE_DB_PER_OCT` (display) — kept equal only by a comment. They matched, so
it was not an active divergence, but they could drift silently.

## The system (context)

Audio flows in three layers. Only Layer 2 had the problem.

- **Layer 1 — Inputs → Sends (already unified).** Mic/line (cpal), system/per-app
  audio (CoreAudio taps), and audio layers (mixer taps) all sum to **one post-gain
  mono stream per send**, analyzed once by one `StreamingSendAnalyzer`. Gain is
  applied here, before analysis — the scope already shows post-gain.
- **Layer 2 — The spectrogram (this work).** Per send: VQT → tilt → floor → column.
  The source of truth.
- **Layer 3 — Consumers (already read Layer 2).** Scope, triggers, slider modulation,
  level meters — all read the per-send features that Layer 2 produces. They are
  **not changed** by this work; we fix the stage they all read from.

The scope shows **one send at a time** (the tapped send). Source-of-truth is
therefore per-send; the other sends run the identical pipeline unseen.

## The unified design

**One resolved floor per send** drives everything:

- It is `AudioSend.floor_db` (the "Floor" stepper). When unset (`FLOOR_DB_OFF`) it
  **resolves to the config default `db_min`** (`-59 dB`) — so an untouched project
  behaves exactly as the display did before (no migration, byte-identical save).
- The floor **is** the colour-ramp bottom (`db_min`). Black on screen = at/below the
  floor. There is no second display floor.
- The floor is applied **in the tilted domain**: a bin is zeroed when its *tilted*
  magnitude's dB is below the floor — the same tilted dB the shader paints and the
  detector reduces. The line you set is the line you see.
- Below-floor bins are **zeroed** in the one column. The detector therefore needs no
  floor knowledge: a zeroed band has zero energy → zero flux → zero amplitude → no
  fire, for free. `ONSET_AMP_GATE` and the `if loud { … }` feature gates are deleted.
- The pink tilt is **one constant** (`SpectrogramConfig.tilt_slope`), read by both
  the detector's `tilt_weights` and the display widget. No duplication.

### Why keep `vqt_raw` as the scope column (not the tilted column)

The display widget stores untilted magnitudes and applies the tilt in-shader; the
hover readout reports true (untilted) level plus a tilt-weighted value. We keep
sending the untilted `vqt_raw` to the scope, but **floored by the tilted-domain
rule** (a bin is zeroed in *both* `vqt_raw` and the tilted `state.col` when its
tilted dB < floor). Because the worker's `tilt_w` multiplier and the shader's
additive-dB tilt are the same slope, `20·log10(state.col)` equals the shader's
painted tilted dB exactly — so zeroing on `state.col` matches what the shader would
draw. The display ramp bottom is fed the same floor, so black = floor. This keeps
the widget's existing tilt + hover behavior and minimizes blast radius.

### Amplitude normalization tracks the floor

`band_reduce` maps a band's RMS through `(20·log10(rms) − db_min) / (db_max −
db_min)`. With `db_min = floor`, amplitude reads `0` at the floor and `1` at
`db_max`. Defaulting the floor to `-59` keeps today's amplitude scale identical, so
the level meters and amplitude-driven modulation are unchanged by default. Raising
the floor raises the bottom of the meter and the modulation range together — the
intended unified behavior (one floor, end to end).

## Layering that stays separate (by design)

The single floor is "what counts as audible," shared with the display. These sit
**on top** of the unified signal and are intentionally *not* floors:

- **Trigger sensitivity** (per route) — how hard a clean hit fires a clip.
- **Modulation depth / response curve** (per binding) — how a feature maps to a param.
- **Gain** (per send) — input trim, applied *before* the spectrogram (the scope
  already shows post-gain).

## Phases

- **Phase 1 — One source for the tilt.** Add `tilt_slope` to `SpectrogramConfig`;
  delete `SCOPE_TILT_DB_PER_OCT` (analysis) and `PINK_SLOPE_DB_PER_OCT` (spectral);
  both read the config. Pure refactor, zero behavior change.
- **Phase 2 — Floor in the tilted domain + floor == db_min.** Resolve the floor
  (`floor_db` if set, else config `db_min`). Zero bins where the tilted dB < floor.
  Feed the same floor to the display as the live ramp bottom (`db_min`) and to
  `band_reduce` as its `db_min`.
- **Phase 3 — Delete the hidden gate.** Remove `ONSET_AMP_GATE` and every `if loud`
  gate; features compute on the floored column directly.
- **Phase 4 — Verify.** Floor-gate test rewritten for the tilted domain; a new
  below-floor-is-silent regression; clippy + the focused audio sweep; a runtime
  pass on a kick-only and a full-mix signal.

## Files

- `crates/manifold-spectral/src/lib.rs` — `SpectrogramConfig.tilt_slope`.
- `crates/manifold-spectral/src/spectrogram.rs` — widget reads `tilt_slope`, live
  `db_min` setter, hover uses the field; delete the const.
- `crates/manifold-audio/src/analysis.rs` — tilted-domain floor, floor resolution,
  delete `ONSET_AMP_GATE` + `if loud`, `tilt_weights` reads config; tests.
- `crates/manifold-app/src/content_state.rs` + `ui_bridge/state_sync.rs` — carry the
  resolved per-send floor to the scope renderer.
- `crates/manifold-app/src/app_render.rs` — feed the live floor as the widget's
  `db_min`; pass `tilt_slope` at construction.
- `crates/manifold-core/src/audio_setup.rs` — floor resolution helper / default.

## Invariants (do not regress)

- The detector takes **no** floor or `db_min` argument that differs from the display.
  One value, resolved once, fed to display + detector + amplitude.
- Below the floor is **zero** in the one column. No algorithm re-implements a floor.
- The tilt slope and db range have **one** definition each.
- `floor_db` keeps its `FLOOR_DB_OFF` sentinel + skip-serialize, so old projects are
  byte-identical; "off" resolves to the config `db_min` at runtime.
