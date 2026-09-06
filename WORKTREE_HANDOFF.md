# Live UI control — paused at Peter's request, 2026-09-06

Branch `codex/live-ui-control`, slot-1. Base `909ad80bc9794361e325da7dea6739ae4c3727a5`.
Follow-up: `BUG-m7nb`.
Do not reset/reacquire this slot. Other Claude agents own their separate worktrees.
No merge to main or landing gate has been performed.

Contract and use: `docs/UI_AUTOMATION_DESIGN.md`, live primary-window section.
Feature `ui-automation` plus a private Unix socket exposes current widget metadata
and normal input-handler gestures. No project mutation API or new thread/lock.
Scripts: `launch_live_ui.py`, `live_ui.py`, `live_ui_generator_demo.py`.

Verified:
- Feature-enabled build and focused app/UI clippy with `--tests -- -D warnings`.
- Nine live protocol/transport/target Rust tests; four Python client tests.
- Feature-matrix coverage and diff checks.
- Live generator demo completed in 3.79 seconds: one generator layer, one clip
  at beat zero lasting 128 beats (32 bars, 4/4), Caustics, speed 0.35, scale 4.50,
  shine 0.60. Clip and parameter undo/redo, playback advance and stop passed.
- Final native screenshot observed correct scene and controls. No screenshots
  were used for decisions during the scripted demo. Demo was not saved; Peter
  closed the optional save dialog. Test process 49472 was closed and exit verified.

Resume work:
1. Review live transport/protocol and shared-input changes; finish hardening as
   warranted. Native-user interruption and disconnect-during-drag have code
   handling but still need live drills. Inspect coordinate/staleness limits.
2. Refresh from current main in this branch, preserving other work; main advanced
   while the session was paused. Check any collisions before validation.
3. Run focused default app/UI tests and existing UI flows through the required
   landing gate. Feature-enabled checks already passed; rerun if code changes.
4. Repeat the demo against the final merged build, update the design status, then
   use `scripts/land_branch.py` for landing. Broader window/accessibility support
   is intentionally deferred, not part of the first workflow.

Temporary evidence (not a permanent artifact contract):
`/private/tmp/manifold-live-demo.json`, `/private/tmp/manifold-live-tests.log`,
`/private/tmp/manifold-live-clippy.log`. These may expire; rerun the scripts.
The ordinary app singleton remains enforced: the launcher must not close or
bypass any other running MANIFOLD instance. An earlier second launch failed
because our original proof app was still running; the native tool then timed out.

Luna implemented the transport skeleton; Astra reviewed/corrected it and added
its tests, app integration, client and demo. No worker landed changes.
