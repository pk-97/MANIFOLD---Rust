# Waveform tempo-map breakpoints — landed 2026-07-17 @ <merge SHA on push>

**Branch:** lane/waveform-tempo-map (Sonnet agent, Fable-orchestrated) · **Level reached:** visual before/after render reviewed by orchestrator (no design doc — single-lane bug fix, found live by Peter minutes after the Liveschool tempo-map import)
**Doc status line (quoted verbatim):** none — no design doc.

## The bug

Audio-clip waveforms were painted with ONE constant seconds-per-beat
(`audio_warped_spb`, `60/settings.bpm × warp_ratio`) while playback
(`AudioLayerPlayback::update`, audio_layer_playback.rs:251-257) integrates the
full piecewise tempo map via `TempoMapConverter::beat_to_seconds_immut`. Every
project until today had a single-point map, so linear was exact; the
Liveschool eval import (98 points, 132–166 BPM) exposed it — peaks drifted
tens of seconds from their audible beats and the assumed window overran the
file, clamping the tail. Sound was always right; only the picture lied.

## The fix (root)

`audio_waveform_breakpoints()` in `crates/manifold-app/src/ui_bridge/state_sync.rs`
bakes, per audio clip at translate time, `(x_frac, file_secs)` pairs at the
clip start, every tempo-map point inside it, and the clip end — mirroring the
playback formula pointwise. Carried as plain `Vec<(f32, f32)>` on
`ViewportClip`/`ClipScreenRect` (ui depends on foundation only);
`warped_secs_per_beat` deleted (no other consumers).
`crates/manifold-renderer/src/clip_content_gpu.rs` draws one `draw_waveform`
segment per breakpoint pair; fingerprint folds in segment geometry + MIP texel
count. Constant-tempo projects produce exactly 2 breakpoints — pixel-identical
to the old rendering.

## Gate results (verbatim)

```
cargo clippy -p manifold-app -p manifold-ui -p manifold-renderer -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 34.33s
cargo nextest run --workspace   (post-merge of origin/main)
     Summary [  11.257s] 3631 tests run: 3631 passed, 13 skipped
```

4 new unit tests beside the producer (constant-map equivalence, 3-point map
vs converter deltas, in_point offset, non-audio empty). The agent's run saw
one failure (`catalog_gen::regenerates_in_sync`) pre-existing at its base and
already fixed on main by the time of landing — gone in the post-merge sweep.

## Deviations from brief

none reported by the agent; orchestrator review confirmed the producer
mirrors playback and the PNGs show the spike landing on the computed mark
(31.25%) with bar density flipping correctly at the 60→240 boundary.

## Shortcuts confessed

Visual probe was a throwaway (rendered, eyeballed, deleted — not committed as
a regression test). Waveform-vs-grid alignment has no automated pixel test;
the unit tests pin the mapping math instead.

## Verification debt

none opened, none carried.

## Click-script for Peter (≤2 minutes)

1. Open `Liveschool Live Show V6 AUDIO.manifold`, look at the Audio 53 clip — expect: waveform transients now sit on the beats they play on across the whole 20 min (previously drifting mid-show, squashed tail).
2. Scrub to ~18:20 (the 164→60→134 dive into Waiting For You) — expect: visible waveform stretch through the dive, matching what you hear.
