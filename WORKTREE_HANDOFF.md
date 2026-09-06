# Live UI control — checkpoint, 2026-09-06

Branch `codex/live-ui-control`, slot-1. Code checkpoint `50b853a97`.
Follow-up `BUG-m7nb`. Do not reset/reacquire this slot. Main is untouched;
other agents' worktrees and main-checkout dirt are preserved. Origin/main was
merged without conflicts (only an upstream design-doc change).

Contract: `docs/UI_AUTOMATION_DESIGN.md`, live primary-window section.
Feature `ui-automation` and private `MANIFOLD_UI_SOCKET` expose shared native
input handlers and on-demand metadata. No direct project setters/new thread/lock.
Scripts: `launch_live_ui.py`, `live_ui.py`, `live_ui_generator_demo.py`,
`live_ui_safety_demo.py`.

Hardened pointer handover: restore original cursor/modifiers, release held input,
refuse an already-held native mouse, revalidate the target before first press/
wheel, interrupt on resize/scale changes. Last-interruption metadata records
reason/frame/held button/remaining events for runtime evidence.

Verified:
- Final feature-enabled clippy (`--tests -- -D warnings`), ten focused Rust tests
  including shared-handler cursor/button/modifier restoration, eight Python tests.
- Standard landing gate: design status, flow selection, deny, ignored-test guard
  and default touched/dependent crate clippy passed. Test leg FAILED:
  `gap_start_probe::gap_start_black_frame_probe` cannot load
  `/Users/peterkiemann/Downloads/fbTest.manifold`: Operation not permitted.
  Probe source is unchanged from origin/main. No named-red landing performed.
- Separate completion run: all 3,015 remaining tests passed. Explicitly excluded
  that recorded failure; nextest reported ten skips including existing skips.
  This does NOT make the landing gate green.
- Explicit existing flows: drag-clip, drag-clip-release-over-inspector,
  select-and-inspect all passed (path selection had selected no flows).
- Live generator workflow on hardened build: 3.78 seconds, one Caustics clip
  start 0/duration 128 (32 bars, 4/4), speed 0.35/scale 4.50/shine 0.60, undo/redo
  and playback/stop passed; screenshot observed. No project was saved.
- Final diagnostic-enforced disconnect probe passed: buttonHeld=true,
  remainingEvents=51; clip rolled back to 128, input/cursor restored, subsequent
  trim64/undo128/redo64/undo128 passed.
- Native Escape interrupted non-editing wait id=native-handover with an explicit
  error; no held mouse/selection, layer/clip intact. This was a wait interruption,
  not a native key injected during a held drag. The held-drag cleanup is covered
  by the separate disconnect probe. `--await-native` now packages the native
  probe in the safety script (extracted after the live probe; unit-tested).

Next:
1. Resolve the fixture-access gate blocker, then use `scripts/land_branch.py`.
   Direct read still returns macOS Operation not permitted, even escalated.
   No merge/push has occurred. Keep BUG-m7nb open until landing. All other
   required checks are green; do not repeat broad checks without new changes.
2. Update this handoff/design status and superseded references when landed.

Peter clarified that the long CUA timings included his wait to approve tool
calls. Do NOT attribute these delays to app/connection performance or Mac use.
Actual approved Escape ran in 0.015s and AX close in 0.797s. Native input still
shares OS focus, so coordinate with Peter when using his desktop.

Exact commands:
`.claude/scripts/with-build-lock.sh cargo build -p manifold-app --features ui-automation --manifest-path "/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-1/Cargo.toml"`
`python3 "/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-1/scripts/launch_live_ui.py"`

Latest test PID 54879 exited after the normal native AX close-button action;
exit was verified. No project was saved. The earlier PID 63179 termination and
PID-matched sentinel cleanup are complete. No test app remains running.
Do not kill other MANIFOLD instances; the ordinary singleton remains enforced.

Temporary evidence (all `/private/tmp/`): `manifold-live-demo-final.json`,
`manifold-live-safety-final.json` (now includes held-button diagnostics),
`manifold-native-handover.json`, `manifold-live-tests-final.log`,
`manifold-live-clippy-final.log`, `manifold-live-landing-gate.log`,
`manifold-live-remaining-tests.log`, `manifold-live-headless-flows.log`.
