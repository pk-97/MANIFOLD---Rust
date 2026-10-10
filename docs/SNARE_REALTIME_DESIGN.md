# Realtime Snare Detector — the trained snare model on the detector worker

**Status:** BUILT on feat/snare-port, awaiting Peter's live test · owed: fast breakbeats and rolls, task 2.1 of BUG-9ngk8 (snare and clap epic) · pointer: docs/KICK_REALTIME_DESIGN.md (shared container, threading, parity rules)
Lifecycle: contract — the snare model's container, parity rules and retrain-to-ship steps; `snare/mod.rs` cites it.

## 1. What ships

A trained snare/clap detector beside the kick, on the same detector worker. One class: the
prominent backbeat snare or clap (rolls count; rimshots and cross-sticks do not). Consumers read
`SendFeatures.bands[Mid].snare`: 1.0 on the fire hop, decaying like the kick. The clip-trigger
drawer's "Snare" chip picks `AudioFeatureKind::Snare` (saved as `"snare"`); an old trigger saved as
Transients×Mid keeps firing from transients and shows as its own chip.

Pipeline, all causal, 48 kHz mono (input of any rate is resampled first, as for the kick):
1. Candidates: the 1–8 kHz band's energy over the last 5 ms of each 256-sample hop rises at least
   4.5 dB over the minimum of the previous 4 hops; one per attack, 6 hops apart; available 8 hops later.
2. One CNN, the kick net's family: 64 causal band envelopes on a 2 ms grid, a 190 ms slice ending
   40 ms after the candidate's emission, max-pooled into four time segments; three input channels:
   shape (slice minus its max), new (minus each band's pre-onset median), and noisiness (per band,
   the spectral flatness of a 2048-point FFT, clipped to −20…0 dB).
3. Fire when the probability reaches the cutoff and the emission is at least 60 ms after the last
   fire's. A fire is stamped at its decision (emission + 40 ms + one frame), as kicks are, so it lands
   on the same hop whatever the render frame size.

No trees and no song-relative stage: on this net they added nothing. Held-out score (28 songs,
cutoff chosen on the other songs, 3 seeds): F1 .678 mean. Fast runs (hits under 200 ms apart) are
unscored and caught about 46%; that is the owed task above.

Cost: 18 ms CPU per second of audio (kick 25 ms). The worker publishes each detector's events as
soon as it has run, so the snare never holds a kick fire past the content thread's 2 ms wait.

## 2. Model and parity

`crates/manifold-audio/assets/snare_model.mkick` (kick container format) holds every number:
candidate filter and rule, flatness window and band bins, the net's band filters and weights,
cutoff, refractory. Goldens: `crates/manifold-audio/tests/fixtures/snare/clip_{dense,quiet_start}.mkick`.
Rust feeds the audio in random 256–1024-sample chunks; candidates and fires must match exactly,
net probability within 1e-4 (measured: 5e-7).

## 3. Retrain → ship

`scripts/dev.py snare-release train` fits the final net on every labelled song and takes the cutoff
from the held-out run (`tools/audio_analysis/eval/snare_net.py`, `SNARE_FLAT=1 SNARE_PAST_MS=190`);
`snare-release export --out crates/manifold-audio` writes the model and goldens; then
`cargo nextest run -p manifold-audio snare`. Labels: `tools/audio_analysis/eval/snare_labels.py`
on the shared toolkit (`detector_labels`, `detector_eval`, `detector_cands`).
