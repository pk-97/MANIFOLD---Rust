# Audio Eval Harness — how to run it, read it, and grade against it

**Status: NORMATIVE working guide · 2026-07-06 · Fable.** The operating manual for
`mod_harness`, the offline grading loop every audio-analysis change (salience,
tracker, presence, transients) must pass through before touching the live path.
Design context: [AUDIO_OBJECT_TRACKING_DESIGN.md](AUDIO_OBJECT_TRACKING_DESIGN.md).
Written so a session with NO prior context can run, read, and judge results.

**2026-10-09 audit note:** the active pack now has 66 corrected visual kick
estimates (14/15/15/12/10); `attack_review.csv` preserves the one duplicate,
four clip-boundary events, and two ambiguous ending hits as provenance and
excluded-event registry. Primary scoring must exclude [0, 0.250] seconds for
`clip_boundary` and [estimated_attack_s - 0.100, EOF] for `needs_listening`,
reporting those categories separately. `bad_guy` has a mix reconstructed from
its unchanged stems and matching label timebases; all previous scores and
per-track calibration are stale. No auditory validation is claimed. See
`tests/fixtures/audio_labels/README.md` and BUG-qtd.

Reproduce the corrected five-full-mix Kick baseline from a worktree:

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo build -p manifold-audio --example mod_harness
python3 tools/audio_analysis/eval/live_kick_baseline.py \
  --harness target/debug/examples/mod_harness \
  --audio-root '/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio' \
  --out-dir /tmp/live-kick-baseline \
  --report /tmp/live-kick-baseline.json
```

This uses the unchanged live `StreamingSendAnalyzer`, default settings, and
native-rate full mixes. It scores one-to-one matches at ±35/50/70 ms (50 ms
primary) and records uncertain/boundary-region triggers separately. Raw
availability is `(kick_hop + 1) * hop_samples / sample_rate`, not the old
zero-based plot time and not a latency-corrected timestamp. The separate
-35/+200 ms association diagnostic measures late nearby triggers and possible
duplicates without improving the tight accuracy score. It does not establish
which sound caused a trigger or measure capture/UI/display delay. Results:
`tools/audio_analysis/eval/scoreboard/live_kick_2026-10-09.json`.

**2026-10-09 attack trial — experimental, not enabled in the app:**
`kick_attack_probe` processes PCM causally through 45–140, 140–400 and
400–2000 Hz filters, 3/80 ms power followers, an attack/rearm check, a
35 ms low-band confirmation timeout and a 60 ms minimum firing interval. This is
the fifth mechanism variant tried on these same clips, with one fixed setting
across songs. These are development results, not held-out validation.

| Full mix | Live → trial matches within ±50 ms | Live → trial extra triggers, allowing -35/+200 ms association |
|---|---:|---:|
| Apricots | 0 → 14 / 14 | 5 → 0 |
| Bad Guy | 3 → 15 / 15 | 55 → 17 |
| Feel the Vibration | 2 → 13 / 15 | 27 → 2 |
| Inhale Exhale | 1 → 11 / 12 | 4 → 14 |
| Tears | 2 → 10 / 10 | 34 → 7 |
| Total | 8 → 63 / 66 | 125 → 40 |

At the strict ±50 ms tolerance, unmatched triggers fall from 179 to 42;
that includes late kicks, so it is not a pure false-positive count. The wider
association finds 65/66 labels versus 62/66 live; median availability delay
falls from 68.17 to 15.67 ms (p90 116.33 to 43 ms). The Rust event hops match
the SciPy reference exactly on all five files. The standalone synthetic probe
reports silence 0, four falling-pitch kicks 4, stationary bass pulses 3 and
slow bass swell 0 events. Bass-note confusion and the Inhale regression block
live replacement. Next work belongs to BUG-5to: discriminate non-kick attacks
without losing this timing improvement, then validate on fresh material.

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo build -p manifold-audio --example kick_attack_probe
target/debug/examples/kick_attack_probe --selftest
python3 tools/audio_analysis/eval/live_kick_baseline.py \
  --detector attack-probe --harness target/debug/examples/kick_attack_probe \
  --audio-root '/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio' \
  --out-dir /tmp/kick-attack-trial --report /tmp/kick-attack-trial.json
```

Recorded evidence: `tools/audio_analysis/eval/scoreboard/kick_attack_trial_2026-10-09.json`.
The report includes working-source and binary hashes; its base revision alone
does not identify uncommitted prototype source.

**Non-kick rejection trial — rejected; original prototype unchanged.**
Offline stem subtraction identifies bass-dependent events in 13/14 scored
Inhale extras: removing bass eliminates a firing within ±30 ms of each original
event. Bad Guy has 16/17 drum-dependent extras and one dependent on bass/others.
These are detector counterfactuals, not proof of instrument identity; resampling
the stems is an offline diagnostic and is not part of the proposed live path.

The tested rule requires low/body power ratio to grow fourfold within 35 ms of
a prototype candidate. It emits at the confirming hop, with no backdating.
Wider-association extras fall from 40 to 9, including Inhale 14→2 and Bad Guy
17→2. However, associated Apricots labels fall from 14 to 7, and Feel's median
availability slips to 58.33 ms. Overall ±50 ms matches fall from 63 to 41/66.
A mandatory low-band descent therefore fails this development pack under the
current provisional visual labels. It must not become a live veto. No thresholds
were swept to repair these results.

Reproduce the diagnosis and fixed-rule comparison with
`tools/audio_analysis/eval/kick_attack_rejection.py --audio-root <audio>
--harness target/debug/examples/kick_attack_probe --out-dir /tmp/kick-rejection
--report /tmp/kick-rejection.json`. Recorded results are in
`tools/audio_analysis/eval/scoreboard/kick_rejection_trial_2026-10-09.json`.

Independent Astra checks also reject this gate: all seven Apricots misses fail
on drums alone at the same candidate times, and similar drum waveforms receive
opposite decisions. No evidence justified removing those labels. Two past-only
48 ms spectral-innovation gates were also rejected: low-band share retained
49/66 timely matches with 36 extras; tonal-concentration rejection retained
59/66 with 34 extras, losing four previously timely kicks. Both retain all
17 Bad Guy extras. Their retained timestamps are unchanged; this is evidence
loss, not an added-delay result. Scratch scripts, JSON and Apricots plots are
preserved locally under
`~/.cache/manifold/audio-rejection-research-2026-10-09/`.

The parallel non-kick code audit found that Snare/Hats/Bass chips map to
Transients × Mid/High/Low, not instrument classifiers. Next evaluations need
separate reviewed snare/hat labels (including coincident hits), and raw-versus-
shaped measurements of synthetic wobble/growl/note changes plus real passages.
Default modulation release is 120 ms; pitch holds during dropout. These need
musical-response checks, not just feature jitter measurements. A separate
static UI/runtime mismatch needs reproduction: Step/Random Kick/Transient
actions bypass Attack/Release while the drawer still offers those controls.

## 1. Running it

```
# All nine synthetic scenarios, one PNG each + numeric gate lines on stdout:
cargo run -p manifold-audio --example mod_harness -- --selftest --out /tmp/st.png

# A real clip (WAV/AIFF/MP3/FLAC; stereo downmixed like the live path):
cargo run -p manifold-audio --example mod_harness -- path/to/clip.wav --out /tmp/clip.png

# Flags: --csv <dir> per-hop data · --floor <dB> analysis floor (default off)
#        --bpm <f> beat/bar gridlines (auto-parsed from "<n>bpm" in the path)
#        --low/--mid crossovers · --start/--dur excerpt
```

CSV filenames embed the input path — pre-create nested dirs when batch-running files
(`mkdir -p <csvdir>/<full/input/dir>`), or fix the papercut (sanitize `label` in
`write_csv`, mod_harness.rs) if it bites again.

**Real fixtures:** `tests/fixtures/audio/<track>_<bpm>bpm/{mix,bass,drums,others,vocals}.wav`
— 5 tracks, Ableton stem splits (gitignored, never commit audio). Rendered PNGs:
`tests/fixtures/audio/renders/`. Folder BPMs are historical and are not validated
export tempos. Grade against the reviewed attacks, not an assumed beat grid.

## 2. Reading the PNG

Top to bottom: title strip (config) · spectrogram · seven feature lanes
(AMPLITUDE, BRIGHTNESS, NOISINESS, LIVELINESS, TRANSIENTS, PITCH, PRESENCE) · time
axis (seconds; bar labels `B1..` when BPM known).

- **Band colors everywhere:** magenta = Full, red-orange = Low, green = Mid,
  blue = High (matches the app scope's legend).
- **Spectrogram overlays:** dotted color traces = per-band brightness centroid;
  small white dots = raw per-hop salience peak (memoryless); solid white line =
  the TRACKER's pitch (the product signal); faint horizontal lines = Low/Mid
  crossovers; color ticks at the bottom edge = transient fires (low lowest);
  vertical faint/bold lines = beat/bar grid when BPM known.
- **In each lane:** dim wide smear = raw per-hop value (its width IS the jitter);
  bright thin line = after the default AudioModShape smoother — what a bound param
  actually receives. PITCH lane draws a band only where that band's presence ≥ 0.25;
  a blank PITCH lane with a good white spectrogram line means presence is failing,
  not tracking.
- Judging: a "connected" result = the white line rides the perceptual object, PITCH
  lane engaged, PRESENCE high where the object exists and ~0 where it doesn't,
  TRANSIENTS ticking on real hits only.

## 3. Reading the CSV

One row per hop (~5.3 ms). Columns:
`hop_index, time_s, ground_truth_f0_hz, salience_f0_hz,` then per band
(`full,low,mid,high`) the five features `amplitude, brightness, noisiness,
liveliness, transients`, then `tracked_f0_hz`, then per band `pitch, presence`.

- `ground_truth_f0_hz`: selftest scenarios only (NaN for kicks/busymix/riser gaps
  and all file inputs). `salience_f0_hz` = memoryless P1 peak; `tracked_f0_hz` =
  the D5 tracker (NaN until first acquisition). `transients` hits exactly 1.0 on a
  fire hop, then decays ~0.85/hop.
- Standard checks (python one-liners):
  octave error = `12*log2(a/b)`; jump count = adjacent `tracked_f0_hz` deltas
  > 6 st; fire rate = count(`*_transients > 0.999`)/duration; on-grid % = fires
  within ±35 ms of the 8th/16th grid at the clip's BPM; presence health = mean
  per band. Copy the scan scripts from the session digest or rewrite — they're
  ten lines.

## 4. The gates (selftest stdout)

`P2 <scenario>:` tracker trajectory gates · `P2b:` presence gates ·
`P2c notes:` note-based material gates · `P3:` transient fire-count gates.
Each line prints its own bound and PASS/FAIL. **Known-failing by design as of
2026-07-06 (post-BUG-042 (onset-settle-grab)/043 fixes):** ONE line — `P2c notes` pitch accuracy
(87.6%, gate 90) — now owned by BUG-045 (gap-ring-down-chase), the residual
mechanism after BUG-042's fix. Notes presence is green (100%). Everything else green is the entry state; a change that reddens any other
line is a regression regardless of what it improves.

## 5. Current state + the open-bug oracles (2026-07-06)

Tracker validated on synthetics (dive one smooth line, growl 0.019 st) and works in
STRETCHES on real material (apricots bass: 3 bars fully engaged, then dies). Mixes
inherit stem behavior — object selection survives polyphony. Vocals stems score the
highest presence (0.30–0.43): near-mono sustained tonal is the easy case. Presence
on real note-based basslines is effectively dark.

| Bug | One line | Oracle |
|---|---|---|
| BUG-045 gap-ring-down-chase | tracker follows the kernel ring-down 2-4 bins down in note gaps; value-trend fix direction + its knife-edge risk recorded in the entry | `notes` accuracy line (87.6/90) |
| ~~BUG-042~~ FIXED 2026-07-06 | position-anchored re-acquire window (accelerated takeover clock); see backlog Fixed entry | notes gates + tears bass are the regression guard |
| ~~BUG-044 (mix-trigger-deafness)~~ FIXED 2026-07-06 | novelty-vs-recent-max dual onset criterion; see backlog Fixed entry | `densemix` gate + feel/apricots/tears mix fire counts |
| ~~BUG-043 (deep-bass-floor-anchor)~~ FIXED 2026-07-06 | apex-masked salience comb + dominance/consistency presence factors (see backlog Fixed entry) | `sub` scenario gates are the permanent regression guard |

**Floor experiment (2026-07-06, 25 clips, off vs −28 dB):** a raised analysis floor
is a TRADE, not a win — transient sensitivity recovers on quiet stems (feel bass
1.3→7.1 fires/s; vocals ~2×), but the dead mixes barely move at −28, BUG-043 is
untouched (the floor content is loud, strengthening the ghost hypothesis), and
presence/continuity mostly WORSEN (floor removes the quiet inter-note residue that
keeps continuation alive; bad bass 1→43 octave jumps). Direction it supports:
per-source/adaptive floor as a design candidate, never one fixed global value; and
never tune the floor to fix one feature without re-running the full scan.

## 6. Protocol for future sessions

1. Never grade on the PNG alone or the CSV alone — the picture finds what the
   numbers didn't know to measure; the numbers stop the picture from lying.
2. Any analysis change: selftest gates green (minus known-failing) → full 25-clip
   scan → read at least the PNGs your change should have moved AND one it shouldn't.
3. New failure class found → new synthetic scenario that reproduces it minimally +
   a gate, THEN fix.
4. Tuning constants: bounded candidates justified by mechanism, plateau demonstrated,
   or don't ship it (the 2026-07-06 presence formula history is the worked example).
5. Bugs found and not fixed in-session go to BUG_BACKLOG with their oracle named.
