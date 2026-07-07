# wave/ableton-transport-sync (P1–P3 + P4 docs) — landed 2026-07-07 @ <merge SHA filled at push>

**Branch:** wave/ableton-transport-sync · **Level reached:** L1 / target L2 (P3) + L4 (P4, Peter-owned)
**Doc status line (quoted verbatim):** "**Status:** P1–P3 SHIPPED 2026-07-07 (same-session build, Fable) · P4 landing complete except the L4 live checklist (§6 P4 demo — Peter-owned, the real acceptance gate)"

Design: `docs/ABLETON_TRANSPORT_SYNC_DESIGN.md` (written, approved, and built in
the same session — Peter: "this is a Fable level problem that gets solved here
in this session"). Replaces every wall-clock echo/suppression heuristic in the
AbletonOSC transport path with a closed-loop pending-expectation state machine.

## Gate results (verbatim)

P1 unit tests (16 = T1–T13 + 3 support):
```
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 143 filtered out; finished in 0.00s
```
P2 failure-catalog harness (8 F-tests, FakeAbleton, injected time):
```
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```
P3 full playback+app sweep (all binaries):
```
test result: ok. 138 passed; 0 failed; 2 ignored  (manifold-playback lib)
test result: ok. 156 passed; 0 failed; 2 ignored  (manifold-app lib)
+ 8 smaller binaries all ok, 0 failed
```
Workspace clippy:
```
cargo clippy --workspace -- -D warnings → Finished `dev` profile … in 9.48s (zero warnings)
```
Negative gates:
```
rg 'Instant::now|SystemTime|UdpSocket' crates/manifold-playback/src/transport_sync.rs → 0 hits
rg 'TRANSPORT_ECHO_WINDOW_SECS|CONFIRM_WINDOW_SECS|pending_play_seek|suppress_is_playing_until|transport_changed_externally' crates/ → 2 hits, BOTH in osc_sender.rs (M4L legacy, out of scope by Peter's ruling)
rg 'suppress_next_transport' crates/manifold-playback/src/ableton_bridge.rs → 0 hits
```

## Deviations from brief
1. D5 gate widened to hold CLK *transport* relay while pending (pause-flap
   during in-flight play-from-cursor). Design doc deviations §1.
2. Relay drain gates on drain-time CLK liveness, not frame-start authority
   (from-idle Ableton play would have been discarded — found by attacking the
   integration story mid-build). Design doc deviations §2.
3. P3 negative-gate rg pattern scoped to AbletonOSC-path files (pattern also
   matched untouched M4L legacy). Design doc deviations §3.
4. P3's L2 no-peer degrade log trace not produced (needs the windowed app) —
   VD-013; the sequence is L1-proven by T7/T8 + f7.

## Shortcuts confessed (rolled up from phase reports)
- P2 (Sonnet agent): MIDI-clock plane modeled as brief specified (engine beat
  follows fake song time when unsuppressed), not a replica of
  `derive_external_beat` internals; `drop_next_outbound` knob unused by any
  F-test (allow-annotated); tolerances hand-derived from fake arithmetic.
- P1/P3 (Fable): dead-reckoning falls back to 120 BPM before the first tempo
  observation (bridge seeds tempo via query on enable, so the window is one
  round trip); `ableton_is_playing`/`ableton_tempo` bridge fields retained for
  potential HUD reads even though the machine holds its own copies.

## Verification debt
VD-013 opened (real-Ableton L4 checklist, Peter-owned — the design's true
acceptance gate). VD-011/012 untouched (different wave).

## Click-script for Peter (≤2 min, the L4 burn-down)
Rig: Ableton + MANIFOLD, AbletonOSC connected, transport sync ON, MIDI Clock
(IAC) ON. Project in AbletonOSC sync mode.
1. Park Ableton's playhead early in the song. In MANIFOLD, click a bar far
   away and press play → **both apps play from MANIFOLD's cursor; no
   jump-back**. SYNC chip: brief amber "ABL…" then green "ABL".
2. Press stop, then play, in Ableton → MANIFOLD follows each within ~a beat;
   no flapping.
3. While both play, scrub MANIFOLD's ruler → Ableton follows; MANIFOLD's
   playhead never snaps back.
4. Drum play/stop rapidly alternating between both apps → settles to the last
   gesture, no oscillation.
5. Automate/ramp tempo in Ableton while playing → lock holds (Link carries
   tempo, CLK carries position).
6. Disable the IAC clock source mid-playback → chip goes amber "ABL no CLK",
   video keeps following (coarser); re-enable → green "ABL".
7. Start a heavy export or renders, repeat steps 1–3 under load → same
   behavior; if a command ever exhausts retries the chip goes red "ABL
   desync" instead of teleporting the playhead.
