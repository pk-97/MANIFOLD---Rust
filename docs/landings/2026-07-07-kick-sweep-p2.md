# Landing — Kick Sweep-Event Detector P2 (runtime integration)

**Date:** 2026-07-07 · **Author:** Opus 4.8 · **Design:** `docs/KICK_SWEEP_EVENT_DESIGN.md` (P2)
**Branch:** `wave/kick-sweep-p2-integration` · **Level reached:** L1 (green tests + exact-match gate); L4 (Peter, live) owed as P3.

## What landed

The BUG-046 successor is now the live Low-band kick detector. `reduce_send`
(`crates/manifold-audio/src/analysis.rs`) OR's a coherent-descending-ridge event into
the Low band's fire decision under the existing shared refractory; the masked-novelty
criterion and every piece of its state are **deleted, not paralleled**.

- New: `KickRidges` + `KickTrack` (analysis.rs) — per-send, Low-band only, all scratch
  pre-allocated (no per-hop allocation). Multi-ridge tracking, rate/extent + track-age
  cap + recent-fire dedup, exactly the P1 spike logic.
- Deleted: `superflux_masked`, `sustain_median_into`, `push_col_hist`, the `col_hist`/
  `sustain_med`/`masked_odf_hist` fields, and the `MASKED_*`/`SUSTAIN_MEDIAN_HOPS`
  constants — 22 use sites, zero remain (`rg` clean).
- Tests: the three masked-novelty tests replaced with three focused `KickRidges` unit
  tests (descent fires once; static/slow/late-bend don't; attack-dedup suppresses body).

## Gate output (verbatim)

Exact-match — `mod_harness` (live `reduce_send`) Low-band fire counts vs the prototype's
`--family ridge-final` reference, all 10 mix/drums fixtures:

```
apricots  mix Low=19   drums Low=26
bad_guy   mix Low=45   drums Low=71
feel      mix Low=27   drums Low=46
inhale    mix Low=38   drums Low=35
tears     mix Low=43   drums Low=31
```
Reference (prototype): mix 19/45/27/38/43, drums 26/71/46/35/31 — **exact match on all 10.**

Guards — `mod_harness --selftest` (exit 0):
```
dive  full_fires=0        kicks    low_fires=8   busymix low_fires=8
riser full_fires=0        densemix low_fires=8   growl   low_fires=0
```
All six fire-gated guards pass (kicks==8 confirms the double-fire fix holds live). The
one FAIL line — `P2c notes 87.6481 (gate >= 90)` — is the pre-existing BUG-045
notes-accuracy line (pitch tracking, unrelated to kicks, non-gating).

Unit tests: `cargo test -p manifold-audio --lib analysis` → 46 passed, 0 failed
(incl. `kick_ridges_fires_on_coherent_descent`, `kick_ridges_ignores_static_slow_and_late_bends`,
`kick_ridge_dedups_against_a_recent_attack`). Clippy: clean.

## Deviations from the brief

- The three masked-novelty tests were **replaced** with `KickRidges` unit tests rather
  than deleted outright: the old ones asserted the removed mechanism on a static-step
  synthetic kick, which the motion-based ridge won't fire on by design. The new tests
  pin each discriminator directly on realistic stimuli.
- The P3 seam-brief size ("Sonnet, one session") was executed by Opus in-session at
  Peter's direction ("continue with wiring it").

## Unmeasured / carried

- **Content-thread `MANIFOLD_RENDER_TRACE` check:** reasoned, not measured. The added
  per-hop work is a bounded peak-pick (≤~110 Low-band bins) plus ≤12 short track
  updates, all allocation-free, dwarfed by the per-hop CQT that already runs. Verifying
  it live needs the full app with an audio-layer send bound — on Peter's running-app
  smoke list, not a landing blocker. → `docs/VERIFICATION_DEBT.md`.

## Click-script for Peter (≤2 min, P3 / L4)

1. Load a bass-heavy finished track (bad_guy-class, no stems) as an audio-layer send.
2. Bind a flash/strobe visual to that send's **Low** transient.
3. Play. Expected: the visual fires on the **kick**, not on every bass note; it does
   NOT go deaf as it did pre-BUG-046. Watch for ~50–65 ms lag on the flash (D7, the
   confirmation latency) — tell me if it reads late and I'll tune `KICK_WIN`.

## Status line (quoted)

> **Status:** IN PROGRESS · P1 (prototype) + P2 (runtime integration) SHIPPED 2026-07-07 · P3 (feel-pass) owed to Peter · 2026-07-07 · Opus 4.8
