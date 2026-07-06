# Audio Eval Harness — how to run it, read it, and grade against it

**Status: NORMATIVE working guide · 2026-07-06 · Fable.** The operating manual for
`mod_harness`, the offline grading loop every audio-analysis change (salience,
tracker, presence, transients) must pass through before touching the live path.
Design context: [AUDIO_OBJECT_TRACKING_DESIGN.md](AUDIO_OBJECT_TRACKING_DESIGN.md).
Written so a session with NO prior context can run, read, and judge results.

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
— 5 tracks, 8-bar grid-aligned loops, Ableton stem splits (gitignored, never commit
audio). Rendered PNGs: `tests/fixtures/audio/renders/`. The clips are ON-GRID once
BPM-warped: fire timing can be judged against the 8th/16th grid (±35 ms).

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
- Standard checks (python one-liners used throughout the 2026-07-06 review):
  octave error = `12*log2(a/b)`; jump count = adjacent `tracked_f0_hz` deltas
  > 6 st; fire rate = count(`*_transients > 0.999`)/duration; on-grid % = fires
  within ±35 ms of the 8th/16th grid at the clip's BPM; presence health = mean
  per band. Copy the scan scripts from the session digest or rewrite — they're
  ten lines.

## 4. The gates (selftest stdout)

`P2 <scenario>:` tracker trajectory gates · `P2b:` presence gates ·
`P2c notes:` note-based material gates · `P3:` transient fire-count gates.
Each line prints its own bound and PASS/FAIL. **Known-failing by design as of
2026-07-06 (post-BUG-042/043 fixes):** ONE line — `P2c notes` pitch accuracy
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
| ~~BUG-044~~ FIXED 2026-07-06 | novelty-vs-recent-max dual onset criterion; see backlog Fixed entry | `densemix` gate + feel/apricots/tears mix fire counts |
| ~~BUG-043~~ FIXED 2026-07-06 | apex-masked salience comb + dominance/consistency presence factors (see backlog Fixed entry) | `sub` scenario gates are the permanent regression guard |

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
   a gate, THEN fix. (That's how notes/riser/growl came to exist.)
4. Tuning constants: bounded candidates justified by mechanism, plateau demonstrated,
   or don't ship it (the 2026-07-06 presence formula history is the worked example).
5. Bugs found and not fixed in-session go to BUG_BACKLOG with their oracle named.
