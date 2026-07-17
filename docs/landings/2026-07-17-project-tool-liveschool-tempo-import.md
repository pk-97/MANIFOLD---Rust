# project_tool CLI + Liveschool tempo-map import — landed 2026-07-17 @ e8bebc8d (+ ffd48102 CLAUDE.md pointer)

**Branch:** lane/project-tool · **Level reached:** end-to-end on the real show file (no design doc — single-session tool lane, no §10 target set)
**Doc status line (quoted verbatim):** none — no design doc; discoverability line added to CLAUDE.md Tooling in the same landing.

## What landed

`crates/manifold-io/src/bin/project_tool.rs` — CLI for agents inspecting and
modifying `.manifold` project files (sibling of `graph_tool`). Verbs: `info`,
`json`, `tempo show/set/at`, `clip add-audio`. Mutations are surgical
raw-JSON edits on the archive's `project.json` — a registry-less typed
load→save round-trip **drops params** ("dropping unknown param id …"), so the
typed loader is used only as a post-edit validation gate; writes go through
`archive::save_v2_archive` (history, dedup, atomic rename — identical to an
in-app save).

First use (the point of the lane): imported the measured live tempo map into
`Liveschool Live Show V6 AUDIO.manifold` — 98 points thinned from the 2,219
Ableton warp markers on the `29 MASTER 9` show recording — and placed that
recording as an audio clip at beat 112 on layer "Audio 53". This file is now
the ground-truth eval set for the upcoming audio features (memory:
`liveschool-audio-eval-set`).

## Gate results (verbatim)

```
cargo clippy -p manifold-io -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.10s
cargo test -p manifold-io
test result: ok. 50 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
(+ 5 smaller integration binaries, all ok)
cargo clippy --workspace -- -D warnings   (main checkout, at landing)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.38s
cargo deny check bans
bans ok
cargo nextest run --workspace
     Summary [  49.847s] 3619 tests run: 3619 passed, 12 skipped
```

End-to-end verification on the real file (production `TempoMapConverter`
via `tempo at`, all 2,219 warp markers):

```
real file vs 2219 markers: worst 2.007 ms · median 1.350 ms
changed paths: ['/settings/bpm', '/tempoMap/points (len 1 -> 98)', '/timeline/layers[0]/clips (len 0 -> 1)']
```

## Deviations from brief

none — brief was conversational (Peter: "built a useful little tool for
yourself that other agents can use to update and modify the save files").

## Shortcuts confessed

- `clip add-audio` refuses overlaps instead of trimming (write-time
  non-overlap is a `Layer` invariant; interactive trimming stays the app's
  job). Deliberate scope line, documented in the bin header.
- `--all-targets` clippy shows two pre-existing failures in manifold-io test
  files (`tests/forward_version_guard.rs` match-single-pattern,
  `loader.rs` items-after-test-module) — not introduced here, not fixed
  here; the landing gate (`--workspace`, no `--all-targets`) doesn't see
  them.

## Verification debt

none opened, none carried. Note (not debt): warp markers end at timeline
beat 2968.5 (20:06 into the 21:15 audio); the final ~90 s of the map ride a
flat 134 BPM assumption from the .als automation.

## Click-script for Peter (≤2 minutes)

1. `cargo build -p manifold-io --bin project_tool` — expect: clean build.
2. `target/debug/project_tool info "<Dropbox>/…/Liveschool Live Show V6 AUDIO.manifold"` — expect: `bpm 132`, `tempo: 98 point(s)`, layer `[0] Audio 'Audio 53' — 1 clip(s), beats 112.00..3123.64`.
3. Open the project in MANIFOLD, play from the top with the audio layer audible — expect: first clips land on the recording's first downbeat and stay locked through the 164→60→134 dive at ~18:20 (the spot where the .als automation alone would drift ~4 s).
