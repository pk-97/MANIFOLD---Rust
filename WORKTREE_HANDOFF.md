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
  including shared-handler cursor/button/modifier restoration, six Python tests.
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
- Live generator workflow on hardened build: 3.68 seconds, one Caustics clip
  start 0/duration 128 (32 bars, 4/4), speed 0.35/scale 4.50/shine 0.60, undo/redo
  and playback/stop passed; screenshot observed. No project was saved.
- Timed disconnect rollback and subsequent trim/undo/redo passed. The subsequent
  strengthened safety flow REQUIRES held-button diagnostic evidence and still
  needs a final live run on the latest build; do not overstate the earlier run.

Next:
1. When Peter's Mac is idle, run the latest generator + strengthened safety flows
   and native-input takeover drill. CUA attachment took 175s and native Escape 38s
   while Peter used the Mac; stop native interactions during his other work.
2. Resolve the fixture-access gate blocker, then use `scripts/land_branch.py`.
   No merge/push has occurred. Keep BUG-m7nb open until these checks and landing.
3. Update this handoff/design status and superseded references when landed.

Exact commands:
`.claude/scripts/with-build-lock.sh cargo build -p manifold-app --features ui-automation --manifest-path "/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-1/Cargo.toml"`
`python3 "/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-1/scripts/launch_live_ui.py"`

Test PID 63179 was verified as our temporary bundle, terminated without taking
focus, and exit verified. Its matching `session.active` marker and private socket
were removed so the test does not leave a false crash notice. Do not kill any
other MANIFOLD instance; the ordinary singleton remains enforced.

Temporary evidence: `/private/tmp/manifold-live-demo-final.json`,
`manifold-live-safety-final.json` (predates diagnostic assertion),
`manifold-live-tests-final.log`, `manifold-live-clippy-final.log`,
`manifold-live-landing-gate.log`, `manifold-live-remaining-tests.log`,
`manifold-live-headless-flows.log` (all under `/private/tmp`).
