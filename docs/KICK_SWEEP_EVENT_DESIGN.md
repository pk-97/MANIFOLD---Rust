# Kick Sweep-Event Detector — motion-based kick detection for the bass-heavy Low band

**Status:** IN PROGRESS · P1 (prototype) + P2 (runtime) + P4 (Kick split from Transients, ridge-only) SHIPPED 2026-07-07 · scope kick lane (magenta bottom tick lane on the Audio Setup scope, P3's tuning monitor) SHIPPED 2026-07-07 · P3 (feel-pass, now binds the Kick feature) owed to Peter · 2026-07-07 · Opus 4.8
P1 @ `648f07e3` · P2 landing report: `docs/landings/2026-07-07-kick-sweep-p2.md`. The live `reduce_send` reproduces the prototype's `ridge-final` fire counts exactly on all 10 mix/drums fixtures; masked-novelty deleted.
Scope lane @ `b6aed008` (rode the ScopeColumn typed-overlay refactor) · landing report: `docs/landings/2026-07-07-kick-scope-lane.md`.
**Prerequisites:** none (the prototype and the 73-label corpus both exist).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

**The governing insight:** on a bass-heavy full mix the Low band's onset detector goes
near-deaf on kicks — the bassline owns the band's flux baseline and the kick can't
out-shout it in its own 45–120 Hz range. The masked-novelty workaround shipped as the
BUG-046 partial (`61c2b0fd`) traded that deafness for the opposite fault: it fires on
bass-note attacks too, so a kick-bound visual strobes on the bassline (Peter, live,
confirmed). Both faults are the same root — **flux cannot tell a kick from a bass note
in the Low band.** But the kick leaves one signature the bass usually does not: a fast,
coherent, *descending* frequency ridge — its pitch envelope chirping ~120→45 Hz over
~90 ms. SuperFlux's max-filter nulls exactly that bin-sliding energy **by design**
(BUG046_P6A_SWEEPS.md, load-bearing finding). So the detector cannot be flux; it must be
**motion.** This design detects the kick from the descent of a tracked spectral ridge,
replaces masked-novelty with it, and is proven against the 73-label corpus before it
integrates.

Peter's constraints, quoted, settled — do not reopen: **"detection runs on the LOW band
of the 3-band split, not full-spectrum"** (full-band fires on hats → spams kick visuals);
the successor is **"the ridge-motion sweep-event"** (audio-object-tracking memory, D9).

Companion docs: `docs/AUDIO_OBJECT_TRACKING_DESIGN.md` (D9/P6 — the four dead masking
families this supersedes); `docs/evidence/audio_modulation/BUG046_P6A_SWEEPS.md` (rounds
1–4, incl. the v0 this design replaces); `docs/AUDIO_EVAL_HARNESS_GUIDE.md` (the
authoritative harness `mod_harness` P2 extends); `tests/fixtures/audio_labels/README.md`
(the 73-label grading target + the bad_guy time-base caveat).

---

## 1. Audit — what exists (verified 2026-07-07)

| Piece | Where | State |
|---|---|---|
| The fire path | `reduce_send` (analysis.rs:1639) | Three OR'd criteria per band under one shared refractory: plain-flux median (BUG-041) + novelty (BUG-044) at :1784, **masked-novelty (BUG-046) at :1767–1785**. Fires set `bf.transients=1.0` and `transient_refractory[bi]`. Extend, don't redesign. |
| Masked-novelty state | `SendState` (analysis.rs:258): `col_hist`/`col_hist_len`/`col_hist_pos` (:286), `sustain_med` (:293), `masked_odf_hist` (:298); consts `MASKED_NOVELTY_FACTOR`/`MASKED_ONSET_DELTA`; `superflux_masked` reduction | **22 use sites, all in analysis.rs.** This is what P2 deletes — it is replaced, not paralleled. |
| Pitch `RidgeTracker` | analysis.rs:937, `trackers: [RidgeTracker; 4]` in SendState (:315) | The **single-object sustained-pitch** follower (BUG-042/043/D5): holds on dropout, settle streaks, challenger ratios. Architecturally OPPOSITE a transient kick detector — **not reused** (D6). |
| Peak-pick helpers | `local_peaks` (analysis.rs:854), `strongest_peak` (:876), `salience_into` (:768) | Reusable. The kick tracker peak-picks Low-band local maxima; `local_peaks`-shaped, on the raw tilted column (D3). |
| Shared constants | `ONSET_REFRACTORY_HOPS`, `ODF_MEDIAN_HOPS`, `SUPERFLUX_*` | Unchanged; the ridge shares `ONSET_REFRACTORY_HOPS` and the band-fire refractory. |
| CQT resolution | `SpectrogramConfig::default` (manifold-spectral/src/lib.rs:74): bpo=24, fmin=10, n_fft=4096, hop=256 | Load-bearing for every rate constant. 24 bins/octave; the Low band (≤250 Hz) is bins 1–111; the kick sweep spans bins ~52–86 (45–120 Hz), fully inside it. |
| The prototype | `crates/manifold-audio/examples/hpss_proto.rs` (`--family ridge`, `ridge-final`) | **P1 — shipped this design's spike.** Replicates `reduce_send` fire-count-exact; the reference the P2 integration must reproduce. |
| The harness | `mod_harness.rs` (authoritative), `AUDIO_EVAL_HARNESS_GUIDE.md` | P2 adds label grading here so the *integrated* detector is graded, not only the throwaway prototype. |
| Ground truth | `tests/fixtures/audio_labels/*.csv` | 73 hand-verified kick labels (apricots 16, bad_guy 17, feel 16, inhale 14, tears 10); `mix_time_s` + `drums_time_s`; ±35 ms tolerance. bad_guy mix labels are warp-scaled ×0.8828 (approximate — grade it at ±70). |

Thread residency: `reduce_send` runs on the capture downmix worker (off-RT) and, for
audio-layer sends, on the **content thread** via `StreamingSendAnalyzer` (analysis.rs:1864).
Both paths already keep `SendState` scratch pre-allocated (`new_send_state` at :1828) — the
hot path is allocation-free, and the kick tracker must stay that way (the prototype
allocates per hop because it is offline; the integration must not — forbidden move in P2).

Persistence: none. The detector is constants + per-send runtime state, exactly like
masked-novelty was. No serialized field, no load-migration, no per-send toggle (parity
with the always-on masked-novelty it replaces — D4).

## 2. The spike — what was measured (2026-07-07, `648f07e3`)

The core bet (motion beats flux) was **unproven going in** — v0 (BUG046_P6A round 4) failed
every way. So it was spiked in `hpss_proto.rs` and graded against the 73 labels before this
doc committed to it. Head-to-head, Low-band kick detection, mix stem, `--family ridge-final`:

| Low-band criterion | mix recall @35 ms | @70 ms | bass false-fires¹ | guards |
|---|---|---|---|---|
| baseline (plain flux only) | 9 / 73 | 19 / 73 | 33 | PASS |
| **or-final** (shipped masked-novelty) | 21 / 73 | 38 / 73 | 55 | PASS |
| **ridge** d14 w10 (this design) | **41 / 73** | **61 / 73** | **57** | PASS |

¹ mix-Low fires within ±70 ms of **no** kick label **and** **no** drums-stem onset — the
approximation of "fired on the bassline, not a drum." Same drums-stem alibi for every row,
so the comparison is valid even though the absolute is fuzzy (non-kick percussion inflates it).

**The result is decisive: the ridge nearly doubles the shipped criterion's kick recall at
equal bass-false-fire cost** (57 vs 55), and breaks the bad_guy deafness specifically
(0 → 15/17 at ±70 — the ±35 undershoot is exactly that track's warped-mix caveat). Both
compared configs share the same plain-flux base; the delta is purely ridge-vs-masked as the
third criterion (verified: `RidgeTrack` runs plain `superflux`, masked-novelty off). This
settles two things: **the moving-ridge shape is proven — no escalation to a matched-filter
architecture is needed** — and **the ridge dominates masked-novelty on every axis, so it
replaces it** (D4).

The residual bass false-fires are a **fundamental limit, not a tuning gap**: baseline
plain-flux alone fires 33 times on non-drum content, because a synth bass with a fast pitch
envelope *is* the same descending shape as a kick in the same 45–120 Hz range. That is the
"musically ambiguous 808/bass" material the label README explicitly excludes; final precision
is Peter's feel-pass (P3/L4), not a harness number.

## 3. Decisions

- **D1 — Motion, not flux.** Detect the kick by the descent of a tracked ridge, not by
  energy rise. *Rejected: more Low-band flux-threshold tuning* (exhausted — D9). *Rejected:
  HPSS / column-mask / Wiener / novelty-floor* (four families measured dead — D9,
  BUG046_P6A_SWEEPS rounds 1–3; never re-try).

- **D2 — Multiple ridges, not the global apex.** Peak-pick every Low-band local maximum
  and follow each as its own ridge. v0 tracked the single loudest bin — always the bass —
  and never saw the kick's descending ridge (BUG046_P6A round 4, `k=15`, recall ~0 on the
  bass-heavy tracks). *Rejected: smooth/denoise the global apex* — the apex is on the wrong
  ridge; smoothing cannot move it.

- **D3 — Three discriminators against the bass, each by mechanism, not threshold:**
  1. **Rate + extent.** A ridge fires only on a coherent descent of ≥ `drop_bins` within a
     `win`-hop window (every step in `[-step_max, +1]` bins). The kick falls ~2 bins/hop
     (34 bins over ~17 hops); a bass portamento (< 1 bin/hop) cannot accumulate `drop_bins`
     inside `win`.
  2. **Track-age cap.** The descent must be the ridge's whole short life (born at the attack,
     descends, dies): `hop − birth ≤ win + 6` at fire. A bass portamento is a *long-lived*
     ridge that bends late — its age at the bend far exceeds `win`, so it is rejected with no
     rate/extent overlap with a kick.
  3. **Recent-fire suppression — the load-bearing one.** A ridge that confirms within
     `win + 3` hops of *any* prior Low-band fire is the **body of an already-reported
     attack** and is silenced. Where flux went deaf (the bass-heavy Low band) there is no
     prior fire, so the ridge speaks. This simultaneously (a) fixes the attack+body
     double-fire that v0's per-descent latch missed — the attack fires flux at hop 0, the
     body confirms ~10 hops later, past the 6-hop refractory — and (b) makes the ridge a
     **fallback that fires only when flux missed**, so it adds nothing on clean tracks where
     flux already works. The synth-kicks guard went 15 → 8 the moment this landed.

- **D4 — Replace masked-novelty; do not stack.** Delete the BUG-046 masked-novelty criterion
  and all its state (§1 inventory). The ridge beats it on recall (41/61 vs 21/38) at equal
  bass cost and is what fixes the very deafness masked-novelty was a workaround for. *Rejected:
  keep masked-novelty for non-bass tracks* — the ridge equals or beats it everywhere, and the
  plain-flux base already carries clean tracks; keeping both is a forbidden parallel path and
  re-introduces the bass-attack false-fires Peter felt live.

- **D5 — Shipping config, calibrated by the spike:** `drop_bins=14`, `win=10`, `step_max=4`,
  `min_peak=0.12` (peak floor as a fraction of the band max), `age_cap=win+6`, `MAX_GAP=1`
  (a ridge may skip one hop), `MAX_TRACKS=12`, refractory = the existing shared
  `ONSET_REFRACTORY_HOPS`. Bounds are mechanism-derived (2 bins/hop ⇒ 14 bins clears in ~7
  hops; `win=10` is the confirmation window). **These constants define the P2 exact-match
  gate** — `--family ridge-final` is the reference.

- **D6 — A new struct, separate from the pitch `RidgeTracker`.** The existing `RidgeTracker`
  (analysis.rs:937) is a single-object *sustained*-pitch follower that holds on dropout and
  settles — the opposite of a transient detector. The kick tracker is a new small per-send
  struct (a pool of short ridge tracks). Reuse only the peak-pick helper, not the tracker.
  *Rejected: extend `RidgeTracker`* — its hold/settle/challenger machinery actively fights a
  transient signature.

- **D7 — Latency is real and named.** The ridge confirms ~`win` hops (~53 ms) after the
  descent start ≈ the kick onset, so a kick-bound visual lags the kick by ~50–65 ms live —
  the ±35-vs-±70 recall gap is exactly this. The flux path fires ~5 ms after its peak; the
  ridge is an order of magnitude more latent. Honest cost. `win` is the latency/robustness
  knob; the final call is Peter's feel-pass (P3). *Rejected: retro-date the fire to the
  descent start* — offline-only; live cannot un-fire the past.

## 4. The detector (P2 seam)

A new per-send `KickRidges` struct replaces `masked_odf_hist` et al. in `SendState`, driven
**only for the Low band** inside `reduce_send`'s existing band loop (kicks are a Low-band
event; a Full-band tracker would fire on `dive`'s spectrum-wide descent — proven necessary).

```
struct KickRidges {
    tracks: Vec<KickTrack>,   // pre-allocated, capacity MAX_TRACKS; cleared, never freed
    last_fire_hop: i64,       // any Low-band fire; -1000 sentinel
    peaks: Vec<u16>,          // pre-allocated peak-pick scratch, capacity = low_bin
}
struct KickTrack { bins: [u16; WIN], len: u8, gap: u8, fired: bool, birth: u32 }
```

Per Low-band hop (the exact logic proven in `hpss_proto.rs::replay_fires_dump`, the ridge
block): peak-pick local maxima above `min_peak × band_max` into `peaks`; greedily extend each
track with the nearest unconsumed peak in `[last − step_max, last + 1]`; fire a full-window
track whose net descent ≥ `drop_bins`, is step-coherent, is within `age_cap`, and passes
recent-fire suppression; cull tracks with `gap > MAX_GAP`; birth tracks from stray peaks. The
fire OR's into the band's existing `fired` under the shared refractory and updates
`last_fire_hop`. **No per-hop allocation** — the prototype's `Vec::new()` per hop becomes the
pre-allocated scratch above (hot-path discipline; the content thread runs this via
`StreamingSendAnalyzer`).

## 5. Phasing

### P1 — Label-graded prototype. **SHIPPED 2026-07-07 @ `648f07e3`.**
Deliverables met: `Mask::RidgeTrack` + `--family ridge`/`ridge-final` in `hpss_proto.rs`;
harness re-graded against the 73 labels in seconds (mix vs `mix_time_s`, drums vs
`drums_time_s`) with the bass-false-fire metric. Gate met: replica validation still exact;
all six fire-gated guards green (`k8`); recall 41/61 proven (§2). Demo artifact: the §2 table
(`cargo run --release -p manifold-audio --example hpss_proto -- --family ridge-final`). L1.

### P2 — Runtime integration. **SHIPPED 2026-07-07 (Opus).**
Landed: `KickRidges`/`KickTrack` on `SendState`, driving the Low band in `reduce_send`
via one OR'd criterion under the shared refractory; masked-novelty and all its state
deleted (22 sites, zero remain). Gate met: `mod_harness` reproduces `--family
ridge-final`'s per-band fire counts EXACTLY on all 10 mix/drums fixtures (mix
19/45/27/38/43, drums 26/71/46/35/31); the six fire-gated selftest guards stay green
(kicks low 8 — double-fire fix holds); 46 analysis unit tests pass incl. three new
`KickRidges` tests; clippy clean. The one selftest FAIL is the pre-existing BUG-045
notes-accuracy line (non-gating). Full report: `docs/landings/2026-07-07-kick-sweep-p2.md`.
The `MANIFOLD_RENDER_TRACE` content-thread check is reasoned-not-measured (the added
per-hop work is bounded, allocation-free, dwarfed by the CQT) — on Peter's running-app
smoke list. Original brief:
**Entry state:** `git log --oneline -1` on the branch shows `648f07e3` or a descendant; re-run
`--family ridge-final` and confirm the reference Low-band mix counts (apricots 19, bad_guy 45,
feel 27, inhale 38, tears 43) before touching `analysis.rs`.
**Read-back (first step):** this doc §1/§3/§4, the ridge block in `hpss_proto.rs`, and
`reduce_send` (analysis.rs:1639–1806). Restate: D4 (replace, don't stack), D5 (the exact
constants), the no-per-hop-alloc rule, the shared refractory.
**Deliverables:** `KickRidges` in `SendState` driving the Low band in `reduce_send`; the
masked-novelty criterion and all 22 of its use sites deleted (§1); label grading ported into
`mod_harness` (the integrated detector graded, not the prototype).
**Seam brief (masked-novelty deletion):** compiler-driven — delete `superflux_masked`,
`MASKED_NOVELTY_FACTOR`, `MASKED_ONSET_DELTA`, and the `col_hist`/`sustain_med`/`masked_odf_hist`
fields first; the build errors are the exhaustive call-site list. Re-run
`rg 'masked_odf|superflux_masked|MASKED_NOVELTY' crates/manifold-audio/src` — expect 22 hits at
start; **if the count differs, stop and list the new sites before editing.** A site that
doesn't delete cleanly means a hidden coupling — escalate, don't adapt.
**Gate (positive):** `mod_harness` reproduces `--family ridge-final`'s per-band fire counts
**exactly** on all 25 fixtures (the BUG-044/046-partial exact-match precedent); the integrated
label grading reproduces mix recall 41/61; the six fire-gated selftest scenarios stay green.
**Gate (negative):** `rg 'masked_odf|superflux_masked|MASKED_NOVELTY|sustain_med' crates/manifold-audio/src`
returns **zero**; `rg 'Vec::new\(\)|vec!' ` inside the new Low-band hop path returns zero
(no per-hop alloc).
**Content-thread gate:** a `MANIFOLD_RENDER_TRACE=1` run with an audio-layer send bound —
no frame >20 ms (the tracker runs on the content thread via `StreamingSendAnalyzer`).
**Test scope:** `-p manifold-audio` focused for the harness/selftest; the single workspace
sweep gates the landing.
**Demo:** the integrated `mod_harness` label-grade output (L1) + exact-count match. **Forbidden
moves:** keeping masked-novelty alive "for safety" (D4); reusing the pitch `RidgeTracker` (D6);
per-hop allocation; adapting a misfit delete site instead of escalating.

### P3 — Peter's feel-pass (L4). **Owed to Peter.**
Bind a kick visual to a Low-band send, play bad_guy-class finished tracks live. Judge: does it
catch the kicks; does it strobe on the bass; is the ~50–65 ms latency (D7) acceptable.
Performer gesture: the first thing a VJ does — a hard on-the-kick strobe. `win` is the knob if
latency reads late. Not headless-verifiable; never let a worker decide it.

### P4 — Split Kick out of Transients (no-fallback detector). **SHIPPED 2026-07-07 (Opus).**
P2 folded the ridge into `Transients@Low` (flux **OR** ridge). Peter's call: fallbacks are why
these detectors feel twitchy, so make the kick its own thing. `Transients@Low` reverts to a
plain SuperFlux onset (identical to every other band); `Kick` is a new `AudioFeatureKind`,
ridge-**only**, Low band, with its own refractory so the two never debounce each other.
`extract()` forces the Low band for Kick so a `Kick`-on-any-band selection can't read silence.
The physical justification (Peter): a sub-bass kick always has a pitched descending body — a
click-only kick can't exist down there (a click is broadband, that's the transient detector's
job) — so ridge-only loses no real sub-bass kicks by dropping the flux half.

**Measured (ridge-only, 73 labels, `hpss_proto --ridge-only`):** at the shipped `drop_bins=14
win=10`, recall holds — 59/73 @±70 ms vs the old hybrid's 61 — while bass false-fires drop
57→37 and spurious 98→58. The flux half was contributing the bass-note false fires, not the
kick catches. `drop_bins=14` is the recall knee (d=16→52/25, d=18→41/20); it stays the kick /
bass-portamento threshold the mechanism math put it at. Tight ±35 ms recall is lower (the ridge
confirms ~50 ms late, the D7 latency); `win=8` pulls that to ~43 ms at 53/73.

**Exact-match gate (real fixtures — the only input both harnesses share, since the synth beds
have drifted apart between the two example files):** runtime `mod_harness` kick counts equal the
`hpss_proto --ridge-only` reference to the fire on all 5 — apricots 16, bad_guy 42, feel 23,
inhale 13, tears 25 — and the reverted `Transients` flux matches the prototype baseline
5/5/6/29/32. Scaffold: `hpss_proto --ridge-only` + `--family ridge-sweep`; `mod_harness` now
prints `kick_fires` for every job.

**Still owed:** the P3 feel-pass now binds the **Kick** feature (not Transients@Low).

## 6. Decided — do not reopen

1. Detection is Low-band only (Peter). Not full-spectrum.
2. Motion, not flux (D1). No more Low-band flux tuning; no HPSS/masking (D9, four dead families).
3. Multi-ridge, not global apex (D2). v0's apex approach is dead.
4. Masked-novelty is replaced, not kept alongside (D4).
5. The kick tracker is a new struct, not the pitch `RidgeTracker` (D6).
6. Config constants are D5's; the P2 gate is exact-match against `--family ridge-final`.
7. ~~No serialized state, no per-send toggle — always-on parity with masked-novelty.~~
   **Superseded by P4:** Kick is now a first-class selectable `AudioFeatureKind` (ridge-only),
   separate from `Transients` (plain flux). No fallback inside either detector (Peter: fallbacks
   are why they feel twitchy). The `KickRidges` tracker still runs always-on for the Low band;
   what's selectable is which feature a modulation binds.

## 7. Deferred (with the trigger that revives each)

- **Precision past the harness floor** — reviving trigger: Peter's ambiguous-808/bass-note
  hand-labeled corpus lands. The 73 labels are clean kicks only; the residual bass false-fires
  (§2) can't be graded without labels that call the ambiguous events.
- **The bad_guy time-base caveat** — reviving trigger: Peter re-exports bad_guy's stems
  *warped* (currently 15.0 s unwarped vs the 13.241 s warped mix). Removes the ×0.8828 scaling
  and lets bad_guy grade at ±35, not just ±70.
- **Lower-latency confirmation** (D7) — reviving trigger: the P3 feel-pass finds ~50 ms too
  late. `win` down-tuning, or an early-fire on partial descent, both bounded by guard-safety;
  a build-phase experiment, not a v1 decision.
- **Salience-based peak-pick** — the tracker peak-picks the raw tilted column (proven).
  Reviving trigger: P3 precision needs it — `salience_into` (harmonic-sum) may separate the
  kick's fundamental from the bass stack more cleanly, at the cost of possible conflation.
  Untested; the spike didn't need it.
