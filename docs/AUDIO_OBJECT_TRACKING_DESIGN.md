# Audio Object Tracking — the dominant voice of a send, as a modulation source

**Status:** APPROVED design, not built · 2026-07-06 · Fable
**Prerequisites:** none (the mod_harness eval loop shipped 2026-07-06 @ `ca9eb490`)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

This is **step 7 of [AUDIO_MODULATION_DESIGN.md](AUDIO_MODULATION_DESIGN.md)** — the
"v2 intelligence" its feature seam was cut for. That doc's §6 commitments (log-space
tracking, onset = segmentation, energy = confidence, per-send extraction) are inherited
here as binding; this doc turns them into mechanics. Its companion,
[AUDIO_OBJECT_INGEST_DESIGN.md](AUDIO_OBJECT_INGEST_DESIGN.md), applies the same core
offline; the relation contract is §8 below.

**The governing insight.** The existing features are instantaneous statistics of a
frequency *band* of the whole mix; a human hears a tracked *object*. Peter (2026-07-06):
*"large supersaw synths pitching down should drive a smooth down modulation that
follows... I'm approaching this from a human perspective where I can single out a full
spectrum bass but still understand how it's moving."* Two structural gaps produce the
jitter and disconnection he describes: no object (a band centroid moves when *anything*
in the band moves), and no trajectory (every hop is estimated fresh; the one-pole
shaper downstream can only trade jitter for lag). The fix is a **salience function**
(which peak is the perceptual fundamental) plus a **causal tracker** (inertia,
hysteresis, hold, confidence) between the VQT column and the features.

**The eval loop is the method.** Peter (2026-07-06): *"We will use your synthetic
tests for now to build the object pipeline. They're good. If it can't pass a synthetic
it won't pass a real mix."* Every phase gates numerically against
`mod_harness --selftest` scenarios (the synth knows its own ground truth, so gates are
machine-checkable), plus a PNG Peter can read. Real clips join the eval set when he
exports them; they refine tuning, they don't gate phases.

---

## 1. Audit — what exists (verified 2026-07-06)

| Piece | Where | State |
|---|---|---|
| VQT column per send, tilted + floored, shared with scope | `crates/manifold-audio/src/analysis.rs` (`form_tilted_column`, `StreamingSendAnalyzer::push`) | Ships. THE input to salience — the tracker must read this exact column, nothing else. |
| Per-band reductions + SuperFlux onsets | `analysis.rs` (`reduce_send`, `band_reduce`) | Ships. Onset per-band fire is the tracker's re-acquire signal (§4). |
| Feature seam: reserved pitch fields | `crates/manifold-core/src/audio_features.rs:45-49` (`pitch_hz`, `pitch_delta_st`, `pitch_confidence`) | Reserved for exactly this since `b186134c` (2026-06-15): "the synchro ridge tracker", confidence gating "so they go still on non-tonal input". Never built, never removed — this design is the planned v2, not a re-proposal. |
| Feature matrix + drawer | `crates/manifold-core/src/audio_mod.rs` (`AudioFeatureKind`, `AudioFeature{kind,band}`), drawer per AUDIO_MODULATION §10.2 (`DrawerSpec`) | Ships. New kinds slot into the existing enum + drawer rows. |
| Legacy serde migration | `audio_mod.rs:160-189` (`LegacyAudioFeature::Pitch/PitchDelta` → `(Amplitude, Full)`) | Safety migration for variants that were never selectable in UI; no real project stores them. Retargeted in P4. |
| Per-send opt-in config | `crates/manifold-core/src/audio_setup.rs:60` (`SendAnalysisConfig.pitch`, serde default false) | Ships, unused. Becomes part of the activation gate (§5). |
| Scope overlay transport | `analysis.rs` (`SCOPE_SCALAR_STRIDE = 7`), `crates/manifold-spectral/src/shaders/spectrogram.wgsl` (`col_scalars`, `centroid_line`) | Ships. Pitch trace extends this stride; precedent is the centroid trace. |
| Output shaper | `audio_mod.rs:276` (`AudioModShape::apply`, incl. `rate_of_change`) | Ships. `rate_of_change` on a log-mapped pitch **is** pitch-delta — see D3. |
| Eval harness | `crates/manifold-audio/examples/mod_harness.rs` | Ships (2026-07-06). Causal replay of the live path, PNG + jitter index; selftest scenarios know their own ground truth. |
| Synchrosqueezing prior art | `plugins/manifold-analyzer-gui/src/spectrum_worker.rs:683` (`synchrosqueeze_into`), `plugins/manifold-analyzer-gui/src/cqt.rs:300` (`process_complex`) | Ships in the Analyzer VST, NOT in `manifold-spectral` (whose `CqtTransform` exposes magnitudes only). Rejected for v1, named revival trigger — D2. |
| Transform resolution | `crates/manifold-spectral/src/lib.rs:74` (`SpectrogramConfig::default`): n_fft 4096 (~85 ms), bpo 24, hop 256 (~5.3 ms), fmin 10 Hz | Half-semitone bins; parabolic interpolation on salience gives ~0.1–0.2 st, ample for modulation (D2 rationale). |

Classification: the transform, the column, the seam, the config, the drawer, the
overlay transport, and the eval loop all **exist**. Genuinely new: the salience
function, the tracker state machine, two enum variants, and one stride bump.
**Extend, don't redesign.**

## 2. Decisions

**D1 — Salience = harmonic summing on the existing VQT column, UNTILTED (amended
2026-07-06 during P1).** Per hop, per send:
`S[k] = Σ_{h=1..5} w_h · col[k + off_h]` where `off_h = round(bpo · log2(h))` (bins are
geometric, so harmonic h is a *fixed* bin offset: 0, 24, 38, 48, 56 at bpo 24) and
`w = [1.0, 0.8, 0.6, 0.45, 0.35]`. A wide supersaw or a growl has energy smeared
around each harmonic; summing across the stack makes the fundamental the dominant peak
even when no single bin is. Cost: ~5 adds × 266 bins + a peak scan per hop — trivial.
**Amendment, with evidence:** the original decision said the *tilted* column; P1
measured that wrong. A harmonic comb is self-similar (the sub-comb at 2^m·f0 is a
subset of the real comb), so salience at octave multiples is always competitive, and
the +3 dB/oct display tilt plus geometric-bin integration hands the top end the boost
it needs to win — dive per-hop hit rate 22.3% tilted vs 66.4% untilted, errors always
+1..+3 octaves ABOVE truth, never below. Salience therefore reads the **untilted,
floored** column (`vqt_raw` post-floor — same floor, so "black = silent" still holds;
the tilt remains a display + band-feature concern). This is the same reason
MELODIA-family systems whiten before harmonic summing.
**Rejected: time-domain mono pitch (YIN/autocorrelation)** — assumes monophonic input;
fails exactly on the full-mix case Peter names, and duplicates spectral machinery.

**D1a — SOTA survey note (web-surveyed 2026-07-06, Peter's challenge recorded).**
Peter asked whether a 2026 neural model should replace the hand-coded architecture.
Survey verdict: **no causal, polyphonic/full-mix pitch or salience model exists at any
budget** — every streaming-capable candidate is monophonic-only. Named findings:
PESTO (real-time 2025 version: mono, <10 ms, ~130k params, LGPL-3.0) is cheap enough
to prototype later as an optional monophonic-send assist behind the same tracker —
Deferred, with the salience seam as its slot; COREPIT (ISMIR 2025 late-breaking,
claims poly+realtime) has no paper or code — watch item, not adoptable. The salience
function is deliberately ONE pure function so a learned replacement can slot in
without touching tracker/features/UI; that seam is the standing answer to "should
this be ML" — re-survey before reopening D1, don't re-litigate from memory.

**D2 — No synchrosqueezing in v1; named revival trigger.** AUDIO_MODULATION §6 called
for a synchrosqueeze port. The audit says its cost buys the wrong thing: synchro
sharpens *frequency precision*, but the failure mode is *which peak is the object* —
that's salience + continuity, which synchro doesn't provide. At bpo 24 with parabolic
interpolation the position estimate is ~0.1–0.2 st — below what a slider mapping makes
visible. **Revival trigger:** if P2's dive gate fails on precision (visible
stair-stepping on slow glides that interpolation can't fix), port `process_complex` +
`synchrosqueeze_into` (anchors in §1) into `manifold-spectral` as a drop-in refinement
of *peak position only* — salience and tracker unchanged. AUDIO_MODULATION §6's status
note should then be updated, not this section rewritten.

**D3 — The drawer gains `Pitch` and `Presence` kinds; delta comes free.** `AudioFeatureKind`
grows two variants (serde `"pitch"`, `"presence"`). `Pitch` = the tracked object's
log-frequency position mapped 0..1 across the selected band's bin window — the *same*
mapping brightness/centroid already use, so the trace and the feature agree with the
picture. `Presence` = tracker confidence 0..1 (D6). **There is no `PitchDelta` kind:**
`AudioModShape.rate_of_change` (`audio_mod.rs:276`) differentiates any feature over
real time, and on a log-mapped pitch that derivative is semitone-proportional — the §6
"semitones/sec" commitment is satisfied by composition. Legacy serde:
`LegacyAudioFeature::Pitch` → `(Pitch, Full)`, `PitchDelta` → `(Pitch, Full)` (the
delta-ness is a shape flag the legacy form never carried; no real project stores these
variants — they were never selectable).
**Rejected: a separate `PitchDelta` kind**, because it duplicates an existing shaper
mechanism and doubles the drawer surface for zero new capability.

**D4 — The band cell scopes the tracker's search window.** `Pitch × Low` tracks the
dominant object *within the low band* (the bass, kick notwithstanding); `Pitch × Full`
tracks the whole spectrum's dominant voice. This reuses the matrix UX unchanged, gives
the performer an instrument-level answer to "which thing?" (drag the crossovers — they
are already draggable on the scope — to fence the object), and bounds the ambiguity
problem instead of pretending to solve source separation. Salience is computed once
per column; each of the 4 windows runs its own cheap tracker over the shared salience.
`BandFeatures` gains `pitch: f32` and `presence: f32` (serde-invisible — `SendFeatures`
is runtime-only, not serialized). The per-send reserved fields `pitch_hz` /
`pitch_delta_st` / `pitch_confidence` are filled from the **Full** tracker (Hz from the
tracked bin's center frequency; delta from its per-hop slew; confidence = presence) so
the seam's original contract is honored and a future HUD readout has real units.

**D5 — Tracker: bounded slew, challenger hysteresis, hold-then-release.** Per window,
per hop, state = `{ pos: f32 (fractional bin), presence: f32, hold: u8, challenger: (bin, u8) }`:
1. Peak-pick salience in the window (local maxima above the floor; parabolic refine).
2. **Continuation:** if a peak lies within `SLEW_RADIUS` bins of `pos`, snap to it
   (bounded by `MAX_SLEW` bins/hop — generous: a 2-octave/s glide at 5.3 ms hops is
   ~0.5 bins/hop; the bound exists to reject teleports, not to smooth).
3. **Takeover:** a stronger peak elsewhere must out-salience the continuation by
   `CHALLENGE_RATIO` for `CHALLENGE_HOPS` consecutive hops before the tracker jumps
   (kills one-hop flicker to a passing element without adding lag to real motion).
4. **Onset re-acquire (AUDIO_MODULATION §6.2):** the band's SuperFlux fire bypasses
   hysteresis for that hop — a new note may legitimately teleport. `prev_raw` in any
   bound shaper sees the jump once; `rate_of_change` users get one spike, which the
   §6.2 segmentation semantics accept (a new note IS an event).
5. **Dropout:** no acceptable peak → `pos` HOLDS (pitch is a position; it never snaps
   to zero), `presence` decays with `PRESENCE_RELEASE`; a peak reappearing near `pos`
   within `HOLD_HOPS` resumes silently. Presence rises with `PRESENCE_ATTACK` while
   tracking (attack < release: trust is earned slowly, lost slowly, so a masked beat
   doesn't strobe the visual).
Starting constants (tuned in P2, committed in code as consts with this doc referenced):
`SLEW_RADIUS 6`, `MAX_SLEW 3.0`, `CHALLENGE_RATIO 1.5`, `CHALLENGE_HOPS 12` (~64 ms),
`HOLD_HOPS 38` (~200 ms), `PRESENCE_ATTACK/RELEASE` as one-pole taus 30 ms / 250 ms.

**D6 — Presence gates pitch, and is itself a feature.** `presence = tracked ridge
salience ÷ window energy` (the §6.3 commitment), one-pole smoothed per D5. The pitch
*feature* freezes on low presence rather than reading 0 (0 is a position). `Presence`
as a drawer kind is the "fade the effect in as the bass asserts itself" control Peter
asked for, and doubles as the debug meter for "why isn't pitch moving".

**D7 — Runs inside `StreamingSendAnalyzer`, gated per send.** The tracker is a new
struct owned by `SendState` — same thread (content), same cadence (per hop), same
lifecycle as the band reductions. Activation per send: `SendAnalysisConfig.pitch == true`
**or** any enabled audio mod on any preset references that send with a `Pitch`/`Presence`
feature (the runtime already resolves send references each tick; it passes the
activation set to the analyzers). Cost is small enough that the gate is about hygiene,
not survival; the gate exists so an untouched project's analysis is byte-identical to
today's.

**D8 — The dive's transient false-fire is in scope (BUG-041).** The kills-list demands
a silent transients lane on a pure glide; today's SuperFlux fires continuously on the
supersaw dive (evidence: `docs/evidence/audio_modulation/selftest_dive.png`,
2026-07-06). P3 hardens the detector against the dive scenario with the harness as
oracle. This is detector tuning (max-filter radius vs. 7-voice detune width at bpo 24,
threshold floor, possibly gating flux through the tracker's known-object motion), not a
redesign; if tuning can't reach the gate, escalate with the sweep results rather than
widening scope.

## 3. Data model (committed)

```rust
// manifold-core/src/audio_features.rs — BandFeatures gains two fields (runtime-only,
// not serialized; SendFeatures is rebuilt every tick):
pub struct BandFeatures {
    pub amplitude: f32,
    pub brightness: f32,
    pub noisiness: f32,
    pub liveliness: f32,
    pub transients: f32,
    /// Tracked dominant-object log-frequency position within this band's bin
    /// window, 0..1 (same mapping as `brightness`). HOLDS its last value on
    /// dropout — gate with `presence`, never read 0 as "low pitch".
    pub pitch: f32,
    /// Tracker confidence 0..1 (ridge salience / window energy, smoothed).
    pub presence: f32,
}

// manifold-core/src/audio_mod.rs — two new kinds, serde camelCase:
pub enum AudioFeatureKind { Amplitude, Centroid, Noisiness, Flux, Transients,
    Pitch,      // "pitch"
    Presence }  // "presence"

// manifold-audio/src/analysis.rs — per-window tracker state, owned by SendState:
struct RidgeTracker {
    pos: f32,            // fractional bin, window-relative
    presence: f32,
    hold: u8,
    challenger_bin: f32,
    challenger_hops: u8,
    active: bool,        // false until first acquisition
}
// SendState gains: salience: Vec<f32> (num_bins, reused scratch),
//                  trackers: [RidgeTracker; 4]  // Full/Low/Mid/High windows
```

Scope scalar stride: `SCOPE_SCALAR_STRIDE` 7 → **11**: the four pitch positions
(`pitch_yfb` per band, global display-y like the centroids, `-1` = hidden by presence
< 0.25) append after the existing 7. The shader gains a `pitch_line` overlay drawn like
`centroid_line` but 2× width, band identity colors; harness draws the same scalars.

## 4. Behavior (committed semantics, interiors free)

- Salience, peak-pick, tracker update, and feature fill run **inside the existing
  per-hop loop** in `StreamingSendAnalyzer::push`, after `reduce_send` (they consume
  `state.col` and the band onset fires it just produced).
- Window edges = the band bin ranges `reduce_send` already uses (crossover retunes
  re-fence the tracker live; a tracked object outside the new window is a dropout,
  not a snap).
- The Full-window tracker fills the per-send `pitch_hz` (center frequency of `pos` via
  `CqtTransform::center_freqs`), `pitch_delta_st` (per-hop slew × hops/sec ÷ bins-per-
  semitone), `pitch_confidence` (= presence).
- Warm-up: trackers stay inactive until `has_prev` (same guard as flux), so the
  zero-padded fade-in never acquires a ghost.

## 5. What this means on stage

Bind a slider to `Bass send → Pitch × Low`: a supersaw dive drags the visual down the
whole glide and back up, because it is the same tracked object, not a per-frame
statistic. Toggle `rate-of-change` on the same binding: wobble pitch-bend becomes a
bipolar motion signal in semitone units. Bind `Presence`: the effect breathes in when
the bass enters and holds through a masked beat instead of strobing. The crossover
handles on the scope are now also the "which object" fence for pitch — a performer
gesture that already exists gains a second meaning, with the pitch trace drawn live on
the scope confirming what's locked. Degradation is designed: when the mix masks the
object, pitch freezes and presence falls — the visual goes still, never twitchy.

## 6. Plausible-wrong architectures, forbidden by name

- **You will want a second transform** (a dedicated pitch FFT, a synchro port, a
  time-domain path) — no. The tracker reads the *exact* tilted, floored column the
  scope draws and the bands reduce. Divergence between what's seen and what modulates
  is the bug class this whole subsystem was built to prevent. (D2 names the one
  sanctioned exception and its trigger.)
- **You will want to smooth the pitch output with the one-pole shaper** to hide
  tracker jumps — no. The tracker must be smooth at the source (D5); the shaper is
  per-binding feel, not a bandaid. A jumpy tracker with heavy smoothing is the current
  system with extra steps.
- **You will want `Arc<Mutex>`** for tracker state or the activation set — no. State
  lives in `SendState` (content thread); activation rides the existing per-tick
  runtime path like crossovers/gain do (lock-free banks, snapshots).
- **You will want to auto-enable `config.pitch` from the drawer silently** — no.
  Activation is the runtime OR-gate (D7); the config flag stays an explicit user
  toggle (panel work, deferred).
- **You will want to skip the riser scenario** because pitch "obviously" has no
  meaning there — the riser is the test that presence semantics exist at all. A
  tracker that chases noise on the riser fails P2, whatever the dive looks like.

## 7. Phasing

Every phase: focused tests (`cargo test -p manifold-audio --lib` + named new tests),
clippy, harness selftest regenerated, and its PNGs + numeric gate results in the phase
report. Workspace sweep in the final phase only. No GPU-feature runs needed until P5
(shader change).

**P0 — Eval set completion.** Add two selftest scenarios to `mod_harness`:
`riser` (filtered-noise sweep, no tonal content — the presence null test) and `growl`
(150 Hz saw, 2 Hz LFO on a resonant-ish spectral tilt + amplitude, approximating
formant motion at constant pitch). Selftest gains `--csv <dir>`: per-hop ground truth
(known f0 curve, or NaN for riser) + feature values, machine-checkable.
*Gate:* six PNGs render; CSV columns documented in the example's header comment.
*Demo:* the two new PNGs — L2. *Forbidden:* reusing the busymix synth as "growl".

**P1 — Salience column, offline-verified. ✅ SHIPPED 2026-07-06 (gate as amended).**
`salience_into(col, bpo, out)` in `analysis.rs` + peak-pick with parabolic refine,
unit-tested (pure functions). Harness: salience peak per hop overlaid on the
spectrogram (small dots, Full window only for now).
*Gate — amended 2026-07-06 with measured evidence:* the original bar (dive per-hop
argmax ≥95%) was mis-calibrated — the dive's 7-voice detune beating genuinely cancels
the fundamental bin for short stretches (measured: 36 miss runs, median 4 hops,
max 35, ZERO exceeding the D5 hold window of 38), which no memoryless per-hop
estimator can beat; temporal integration is the tracker's job by design. Amended gate:
dive — per-hop argmax within ±2 bins ≥60% AND max consecutive-miss run ≤38 hops;
growl — ≥95%; riser — no gate. **Shipped numbers: dive 66.4% / max run 35 (naive
tilted baseline 20.8%); growl 100%.** The ≥95%-of-hops smoothness bar is enforced at
P2's tracked-trajectory gate, unchanged. Executor note: P1's failure → diagnosis →
D1 amendment (untilted column) is recorded in D1; do not re-try tilted input.
*Demo:* dive PNG with the dot-trace riding the fundamental — L2. ✅
*Forbidden:* normalizing salience per hop (kills the presence ratio later); subharmonic
"corrections" bolted on before the tracker exists.

**P2 — Tracker + features + harness lanes. ✅ SHIPPED 2026-07-06 — ALL gate lines
PASS after P3 landed the same day** (dive max Δ 0.383 st / mean 0.057 / 100% within
±1 st; wobble stddev 0.318 st; kicks/riser/growl as below). The paragraph that follows
records the mid-phase state for archaeology: P2's code shipped with dive/wobble
blocked on BUG-041, and P3's sweep alone closed both — the D5 step-4 softening was
NOT needed. The D6 presence-scale recalibration (finding 2 below) remains OPEN, owed
before P4 exposes Presence in the drawer. Shipped: D5 tracker
(all six unit-tested behaviors), BandFeatures pitch/presence, `set_pitch_tracking`
(default off; other five features bit-identical when off — tested), harness lanes +
CSV + self-printing gate lines. Passing: kicks (0% spurious low-presence), riser (100%
presence-null, 1 acquisition), growl (0.019 st stddev — the tracker itself is sound).
Failing: dive (max Δ 24 st, 82.5% within ±1 st) and wobble (7.25 st stddev) — every
discontinuity co-occurs with `full_transients == 1.0` in the CSV: BUG-041's false
fires reach the tracker through D5 step 4's unconditional re-acquire bypass, which no
D5 constant can bound (verified: CHALLENGE_HOPS 6/12/20 leave max Δ unchanged).
**Therefore P3 is a prerequisite for P2's sign-off, not an independent phase — P3's
exit gate now includes re-running these P2 lines to PASS.** Two findings for that
session, recorded so they aren't rediscovered: (1) if detector hardening alone doesn't
close wobble (its LFO re-attacks are arguably *genuine* onsets), the sanctioned D5
amendment is to keep CHALLENGE_RATIO on onset hops and drop only the time requirement
— an onset lets a *dominant* new peak in immediately, never a merely-present one;
(2) D6's presence denominator (window energy) is mis-scaled — a perfectly tracked
growl reads 0.02–0.08, far under the 0.25 display bar, so presence is not yet a usable
performer signal; recalibrate (candidate: salience-concentration ratio — tracked peak
÷ total window salience) with the riser gate as the regression guard. Original brief
follows. D5 state machine, 4 windows, D4/D6
feature fill, per-send reserved fields, warm-up guard, activation OR-gate (D7 —
runtime side may land as always-on-in-harness + config-gated-in-app if the runtime
wiring is thin; the byte-identical-when-inactive property is the gate). Harness: PITCH
and PRESENCE lanes (same band colors; pitch lane draws only where presence ≥ 0.25) +
jitter rows for both.
*Gate (numeric):* dive — Full/Low pitch trace: zero discontinuities > 1 semitone
between adjacent hops after acquisition, mean |Δ| ≤ 0.15 st/hop, tracks the known
curve within ±1 st over ≥95% of hops; wobble — pitch stddev ≤ 0.5 st across the clip
(amplitude wobbles, pitch doesn't); riser — Full presence ≤ 0.15 on ≥90% of hops,
pitch holds (no chatter: ≤ 2 acquisitions across the clip); kicks — Low presence: no
sustained acquisition (≤ 20% of hops).
*Demo:* all six PNGs — L2, Peter reads them against the kill-list.
*Forbidden:* smoothing pitch anywhere but the D5 state machine; reading `latest()`
features into the tracker (it consumes the column, not its own outputs).

**P3 — Onset hardening vs. the dive (BUG-041). ✅ SHIPPED 2026-07-06.** Sweep of ~150
configs found the threshold, not the max-filter, was the defect: `SUPERFLUX_THRESH_FACTOR`
2.0→7.0, `SUPERFLUX_DELTA` 3.0→48.0 (real kicks survive delta 30–300; 48 sits
mid-plateau); radius and lookback unchanged (no measurable effect). Dive/riser/growl
0 false fires, kicks exactly 8, busymix 8; P2 gates all green with no D5 softening.
⚠ Tuned on synthetics — the stricter threshold applies to the live Transients feature
everywhere; validate soft-onset material against Peter's reference clips (VD: the
one open verification debt of P2/P3). Original brief follows. Parameter sweep in the harness
(max-filter radius, `SUPERFLUX_DELTA`, lookback) against dive/kicks/busymix CSVs;
commit the winning constants with the sweep table in the phase report. If no point in
the sweep passes, escalate with the table — do not invent new detector architecture in
this phase. (2026-07-06 survey: nothing published beats SuperFlux on electronic
material; madmom's `CNNOnsetProcessor` defaults are the sanctioned second opinion if
escalation happens.)
*Gate:* dive — 0 transient fires after warm-up; kicks — exactly 8; busymix — ≥ 7 of 8
kicks fire in Low.
*Demo:* dive + kicks PNGs — L2. *Forbidden:* per-scenario constants; gating the
detector on tracker state (coupling direction is tracker←onset per §6.2, never both
ways in v1).

**P4 — Modulation surface + serde.** `Pitch`/`Presence` kinds, drawer rows (two more
entries in the existing feature `DrawerSpec` — precedent §10.2), legacy migration
retarget (D3), activation set runtime wiring if P2 deferred it.
*Gate:* round-trip (STANDARD §5): save a project with a `Pitch × Low` mod → reload →
feature still drives (assert modulated effective value changes over a synthetic feed)
— as an integration test, not a claim; serde name test (`"pitch"`/`"presence"`
round-trip + legacy `{"pitch":...}` migration); negative — `rg 'PitchDelta'` in
non-migration code = 0 hits.
*Demo:* drawer showing Pitch/Presence on a real slider, screenshot or ui-flow if the
drawer is reachable (`scripts/ui-flows/select-and-inspect.json` precedent) — L3 target,
L2 floor with a VERIFICATION_DEBT line.
*Performer gesture:* bind Bass→Pitch to a visible param, play a slide — the param
follows without steps.

**P5 — Scope overlay.** Stride 7→11 through the whole path (analyzer buffers, runtime
drain, `ContentState`, shader storage buffer, `pitch_line` in `spectrogram.wgsl`),
naga parse/validate test updated (precedent: the centroid-overlay WGSL guard,
AUDIO_MODULATION §10.0.3).
*Gate:* naga test green; harness and scope draw the same scalars (harness PNG is the
reference); workspace sweep (final phase).
*Demo:* app scope showing the pitch trace riding a played bassline — L4 (Peter, live;
this is the "what you see is what modulates" moment). L2 floor: harness PNG parity.
*Forbidden:* a second scalar ring; re-deriving pitch display-side from `pitch_hz`.

## 8. Relation to the offline pipeline (contract)

One core, two regimes. The salience function and tracker built here are the **only**
implementation — [AUDIO_OBJECT_INGEST_DESIGN.md](AUDIO_OBJECT_INGEST_DESIGN.md) runs
the same code non-causally (forward pass + backward smoothing + segmentation into
note/gesture events) over demucs stems for timeline ingest. The offline side may ADD
passes over the causal output; it may never fork the salience math or maintain a
second tracker. Anything the offline work needs from this crate (e.g. exposing the
per-hop tracker trace) is an additive API on `StreamingSendAnalyzer`, designed there,
built against this doc's invariants.

## 9. Decided — do not reopen

1. Salience = harmonic summing on the existing tilted/floored VQT column (D1).
2. No synchrosqueezing in v1; revival trigger = P2 precision gate failure (D2).
3. No `PitchDelta` feature kind — `rate_of_change` composes it (D3).
4. Band cell = tracker search window; 4 windowed trackers over one salience (D4).
5. Pitch holds on dropout; presence gates; 0 is a position, never a null (D5/D6).
6. Tracker lives in `SendState`, content thread, per-hop, activation OR-gate (D7).
7. Onset→tracker coupling is one-directional in v1 (P3).
8. The dive/wobble/riser/kicks numeric gates are the acceptance bar; synthetic-first
   per Peter's directive (intro quote).

## 10. Deferred (with revival triggers)

- **Object-scoped amplitude / brightness / texture** (energy over the tracked
  harmonic stack) — after P2 ships and the tracker proves stable on real clips; needs
  its own short design (stack masking has real ambiguity).
- **Wobble rate + phase extraction** (oscillation tracker on object envelopes; the
  phase-locked-LFO stage payoff) — after object amplitude exists.
- **Multi-object per window / object birth-death events as triggers** — revive with
  the ingest design's segmentation work, where birth/death is load-bearing.
- **Synchrosqueeze precision port** — D2's trigger.
- **PESTO as a monophonic-send assist** (D1a) — revive only if a clean mono send
  (isolated bass channel) measurably out-demands the salience path; slots into the
  salience seam, LGPL-3.0 linking implications assessed at that point.
- **Per-send analysis toggles in the Audio Setup panel** (the deferred v1 polish item,
  AUDIO_MODULATION §11.5) — revive with P4 if trivial, else stays panel backlog.
- **Real-clip eval fixtures** — when Peter exports them; they extend the CSV/PNG set,
  gates stay synthetic.
