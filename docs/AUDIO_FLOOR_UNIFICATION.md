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

## The design

**The floor is a GATE, not a contrast knob.** This is the load-bearing correction.
An early version merged the floor into the colour-ramp bottom (`db_min`) so "black =
floor." That recoloured the whole spectrogram when you moved the floor — lowering it
stretched the ramp and blew every colour out hot; it behaved as a brightness control,
not a noise gate. The floor and the colour contrast are **different concepts** and
must stay separate.

So:

- **One floor per send** — `AudioSend.floor_db` (the "Floor" stepper). Unset
  (`FLOOR_DB_OFF`) resolves to `db_min`; clamped to `db_min` so it can't go below the
  ramp bottom (below that, content would paint black yet still be live to the detector
  — the mismatch we're killing). No migration, byte-identical save.
- The floor's **only** job is to **zero** the column below it, in the **tilted
  domain** (a bin is zeroed when its *tilted* dB < floor, the same tilted dB the
  shader paints) — in BOTH `vqt_raw` (scope) and `state.col` (features). A zeroed bin
  paints black (mag 0) and contributes nothing to detection. So black on screen =
  zeroed = silent to every algorithm — *without* the floor ever touching the colour
  ramp. Raising the floor blacks out the bottom cleanly; the colours above don't move.
- **`db_min`/`db_max` are FIXED contrast** (−59 / 0), used by the display shader and
  by `band_reduce`'s amplitude. They are not a floor and never move with the stepper.
- The detector needs no floor knowledge: a zeroed band has zero energy → zero flux →
  zero amplitude → no fire, for free. `ONSET_AMP_GATE` and the `if loud { … }` gates
  are deleted; the centroid scope trace hides on `r.energy == 0`.
- The pink tilt is **one constant** (`SpectrogramConfig.tilt_slope`), read by both the
  detector's `tilt_weights` and the display widget. No duplication.

### Why keep `vqt_raw` as the scope column (not the tilted column)

The display widget stores untilted magnitudes and applies the tilt in-shader; the
hover readout reports true (untilted) level plus a tilt-weighted value. We keep
sending the untilted `vqt_raw` to the scope, but **floored by the tilted-domain
rule** (a bin is zeroed in *both* `vqt_raw` and the tilted `state.col` when its
tilted dB < floor). Because the worker's `tilt_w` multiplier and the shader's
additive-dB tilt are the same slope, `20·log10(state.col)` equals the shader's
painted tilted dB exactly — so zeroing on `state.col` matches what the shader would
draw. This keeps the widget's existing tilt + hover behavior and minimizes blast
radius. The display ramp bottom stays FIXED — the floor only blacks out via zeroing.

### Amplitude normalization uses the fixed contrast

`band_reduce` maps a band's RMS through `(20·log10(rms) − db_min) / (db_max −
db_min)` with the **fixed** `db_min` (−59), so the level meters and amplitude-driven
modulation have a stable scale that doesn't shift when you move the floor. The floor
still gates them — it zeroed the sub-floor bins, so the RMS only sees above-floor
energy — but the *scale* is fixed. Floor = gate; `db_min`/`db_max` = contrast.

## The onset ODF reads the dB you see (not linear magnitude)

The same "what you see is what triggers" principle has a second edge. The SuperFlux
onset detection function originally differenced **linear** VQT magnitude
(`ds = m − prev_max`), but the spectrogram paints **dB**. A loud sustained note is a
flat horizontal line on the dB scope — zero change to the eye — yet in linear
magnitude its natural wobble scales with its absolute level, so it threw large flux
and machine-gunned onset ticks under steady harmonic content. (Textbook SuperFlux runs
on log/dB magnitude precisely to avoid this; ours had the frequency max-filter but not
the log domain.)

The ODF now differences the **same dB the shader paints**:
`ds = clamp(20·log10(m), db_min, db_max) − clamp(20·log10(prev_max), db_min, db_max)`.
This makes the ODF **loudness-invariant** — a given *fractional* level step produces the
same ODF whether the band is loud or quiet — so a flat line reads ~0 regardless of
level, and only genuine attacks (big dB jumps) clear threshold. A flat horizontal line
on screen = zero change = no trigger, literally. Only the fire *decision* changes; the
`transients` impulse fed to modulation stays binary, and the threshold is relative
(`avg · SUPERFLUX_THRESH_FACTOR`), so nothing needed re-tuning. The plain liveliness
flux stays linear — it is already loudness-normalized by dividing through band energy.

Remaining tick density on a real **master** mix is genuine musical onset density (a full
mix has constant attacks); the levers for that are per-route trigger sensitivity and a
stronger windowed peak-pick — not the ODF domain, which is now correct.

## Layering that stays separate (by design)

The single floor is "what counts as audible," shared with the display. These sit
**on top** of the unified signal and are intentionally *not* floors:

- **Trigger sensitivity** (per route) — how hard a clean hit fires a clip.
- **Modulation depth / response curve** (per binding) — how a feature maps to a param.
- **Gain** (per send) — input trim, applied *before* the spectrogram (the scope
  already shows post-gain).

## Phases (as shipped)

- **Phase 1 — One source for the tilt.** Add `tilt_slope` to `SpectrogramConfig`;
  delete `SCOPE_TILT_DB_PER_OCT` (analysis) and `PINK_SLOPE_DB_PER_OCT` (spectral);
  both read the config. Pure refactor, zero behavior change.
- **Phase 2 — Floor in the tilted domain.** Resolve the floor (`floor_db` if set,
  else config `db_min`; clamped ≥ `db_min`). Zero bins where the tilted dB < floor in
  both `vqt_raw` and `state.col`. `band_reduce` uses the **fixed** `db_min`.
- **Phase 3 — Delete the hidden gate.** Remove `ONSET_AMP_GATE` and every `if loud`
  gate; features compute on the floored column directly.
- **Phase 4 — Decouple correction.** A first cut fed the floor to the display as a
  *live* `db_min`, which made the floor recolour the whole spectrogram. Reverted:
  `db_min` is fixed contrast; the floor only zeros. The live-`db_min` plumbing
  (widget `set_db_min`, `resolved_floor_db`, `ContentState.spectrogram_floor_db` and
  its wiring) was removed.
- **Tick-smear fix (same pass).** The scope onset ticks marked the decaying impulse
  (~5 columns/fire), smearing the true rate into a carpet. Now the scope marks only
  the fire instant (`transients > 0.999` → 1 column); modulation still reads the
  decaying impulse.

## Files

- `crates/manifold-spectral/src/lib.rs` — `SpectrogramConfig.tilt_slope`.
- `crates/manifold-spectral/src/spectrogram.rs` — widget reads `tilt_slope`; `db_min`
  is FIXED contrast; delete the tilt const.
- `crates/manifold-audio/src/analysis.rs` — tilted-domain floor (zeros only, clamped
  ≥ `db_min`), `band_reduce` uses fixed `db_min`, delete `ONSET_AMP_GATE` + `if loud`,
  `tilt_weights` reads config, fire-instant scope ticks; tests.
- `crates/manifold-app/src/app_render.rs` — pass `tilt_slope` at construction;
  `db_min` stays the config default (no live floor coupling).

## Invariants (do not regress)

- The floor is a **gate** (zeros the column); it never moves the colour ramp. Moving
  the floor must not recolour the spectrogram.
- `db_min`/`db_max` are **fixed** contrast, shared by the display and `band_reduce`.
- Below the floor is **zero** in the one column. No algorithm re-implements a floor;
  the floor is clamped ≥ `db_min` so black-on-screen always equals zeroed.
- The tilt slope and db range have **one** definition each.
- `floor_db` keeps its `FLOOR_DB_OFF` sentinel + skip-serialize (byte-identical old
  projects); "off" resolves to `db_min`.
- Scope onset ticks mark the fire instant only; the decaying impulse stays for modulation.
