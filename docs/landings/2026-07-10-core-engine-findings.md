# Landing report — CORE_ENGINE_FINDINGS F1–F4 + F6

**Queue:** `docs/CORE_ENGINE_FINDINGS.md` (derived from CORE_ENGINE_MAP §13) · **Orchestrator:** Opus · **Date:** 2026-07-10
**Model of record:** fixes built by Sonnet workers in one shared worktree (`.claude/worktrees/core-engine-findings`, off `cc4eeb37`); every fix re-verified by the orchestrator against the worktree manifest before landing.

The P0/P1 tranche of the core-engine work queue: two broken sync features restored, the
external-sync stack given its first test net, and a wrong-clock bug fixed inside the
playback authority — each landing with the pinning test that would have caught it.

---

## What shipped

| # | Pri | What it does on stage | Commit |
|---|---|---|---|
| F1 | P0 | OSC/SMPTE timecode receive works — a set can lock video to Ableton timecode again (was silently dead) | `cf1f3dc6` (+`fd519bdf`) |
| F2 | P0 | MIDI pad launches snap forward to the quantize grid instead of landing where the finger hits; audio triggers stay instant | `04301b37` |
| F3 | P1 | 35 tests over the external-sync stack (was zero) — the net F4/F6 land on | `e4f51459` |
| F4 | P1 | Clip-start timing gates read the real clock, not a zero epoch — no first-frame settle glitch | `48f3a259` |
| F6 | P1 | `suppress_next_transport` staleness — RESOLVED, no new code (live path pre-fixed 2026-07-07; setter pinned by F3) | — |

### F1 — OSC/SMPTE timecode receive (`cf1f3dc6`, clippy `fd519bdf`)
Shared pending slot (mirrors `OscParamRouter`) written by the receiver subscription,
drained in `tick_sync_controllers` before `osc_sync.update()`; subscribe on `enable_osc`,
clear on `disable_osc`. The worker found and fixed two deeper causes the read missed:
`enable_osc` had **zero callers** (now enabled on `OscSyncMode::M4L`, idempotent), and the
default address `"time"` lacked a leading `/` so `rosc` rejected every packet (`"/time"`).
Proof: two UDP-socket integration tests, `tests/osc_timecode.rs`. **SMPTE-over-OSC**, not
raw MTC over MIDI. Owed: a real Ableton/LiveMTC lock on the rig (Peter).

### F2 — MIDI launch quantization (`04301b37`)
Ceil-forward snap in `compute_trigger_snap_beat`'s fallback arm via
`SessionRuntime::ceil_to_boundary` (promoted `pub(crate)` — one ceil function). An
`apply_launch_quantize: bool` threaded through the shared trigger path keeps audio-transient
one-shots immediate; the worker also fixed a pre-existing bug where the `beat_stamp` branch
quantized audio one-shots off-grid. Tests in `tests/live_clip.rs` incl. a non-120-BPM case.
The dead MIDI tick-queue subsystem was left for its own removal pass (**BUG-089**). Owed:
pad-launch tightness feel (Peter).

### F3 — external-sync test net (`e4f51459`)
35 tests: arbiter source×authority gate matrix + ownership/seek-cooldown thresholds;
`MidiClockState` pack/unpack + BPM estimator + SPP reset; CLK/OSC nudge-vs-seek; drop-frame
*pattern*; OSC encode/decode round-trip; engine drift-correction + custom-loop integration
(real `PlaybackEngine`, `StubRenderer`). Guard honored: pinned only map-ruled behavior;
logged the drop-frame divisor discrepancy as **BUG-091** instead of enshrining it.

### F4 — wrong-clock epoch in `sync_clips_to_time` (`48f3a259`)
The single `start_clip` call in the start loop now passes `Seconds(self.last_realtime_now)`,
and the to-stop dirty-deadline anchors off `last_realtime_now` (mirroring the seek path),
so all three timing gates (`recently_started_times`, pending-pause, to-stop) read the real
clock. Sibling zeros (initializers, resets, `.max` clamps, the intentional `stop()` sentinel)
audited and correctly left. Worker confirmed the bug empirically (revert → `0.0` vs `1.98`).
Test: `tests/sync_engine_integration.rs`.

### F6 — RESOLVED, no new code
The live AbletonOSC path was already fixed 2026-07-07 (per-frame clear at
`content_thread.rs:644-645`); F3 pins the arbiter setter contract (`sync.rs:298`). Residue is
the retired M4L `OscPositionSender`, deferred with that sender's deletion
(ABLETON_TRANSPORT_SYNC_DESIGN §8). No content-thread harness exists; building one for a
dead path is unwarranted.

## Verification

Each fix re-run by the orchestrator on the branch tip against the worktree manifest — not
trusted from the worker's paste. Full `manifold-playback` suite green at each step (278
tests by F4). Workspace gate at landing: see the landing commit. GPU-proofs not run — no
fix touches render-timing paths.

## Deferred / owed

- **F5** → Fable design pass. Verdict runtime; per-site authored-vs-live scoping in
  `docs/CORE_ENGINE_F5_SCOPING.md`. Not a mechanical sweep — ~80 bpm reds want the authored
  anchor; only display reads redirect.
- **F13** → awaiting Peter's ruling (split display/rate tempo, or bless). Gates no code.
- **F7** (hot-path allocs + O(active×total) scan) → dedicated perf pass; must be profiled on
  the Liveschool fixture before/after, which end-of-session couldn't do honestly.
- **BUGs logged, unfixed:** 087 (timecode startup false-positive), 088 (pre-existing
  audio_mixdown `--tests` clippy debt), 089 (dead tick-queue subsystem), 090 (audio_mixdown
  test flake), 091 (drop-frame divisor).
- **Live-hardware (Peter's rig pass):** F1 Ableton timecode lock; F2 pad-launch feel.

This landing also brings the timecode receive path to a wired state, satisfying the hard
gate on the timecode-IO design work (Opus prompt-pack Prompt 10).
