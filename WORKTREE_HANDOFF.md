# Water review fixes — 2026-09-10

Branch `wave/live-water`, slot-7. Fixes start from `3ed37813198d6c4adbc74d20806ab8939731f708`. Do not land this feature on main while numerical acceptance remains open (BUG-01vr). Peter requested a full review, removal of camera animation, an active interaction demo, and economical Luna implementation with one lead.

Implemented: stationary camera; repeat pour with downward birth velocity; a gentle repeated cube dip; visible tank aligned to solver bounds; valid small floor meshes; camera-space refraction; seed/count consistency for the bundled preset; full stress-tail preservation; separate emitter/impulse candidate buffers; basin position correction; rejected unsupported collider motion; in-place substep CPU state updates; paused trigger observation outside zero-tick repeat bodies; asynchronous GPU fault reporting and clock freeze; explicit export overload failure; active-frame filtering of cached fatal errors; RT requests use raster water with a transition warning. Both preset descriptions reflect the new behavior.

Faulted collider restoration intentionally uses the last verified frame pose when asynchronous status completes. It is not atomic with same-substep GPU particle acceptance. Ring slots retain generation/ticket ownership until completion. No live GPU waits were introduced.

Validation evidence:
- Production capture: `/tmp/manifold-water-fixed-demo-v3/water_000000.png`, `water_000090.png`, `water_000180.png`. 181 frames at 640x480, stationary camera, visible pour/cube interaction, no reported fault. This is a three-second observation, not a long-run/performance proof.
- Focused RT fallback Metal proof passed: `/tmp/water-rt-fallback-proof.log`.
- Final clippy passed: `/tmp/water-final-clippy.log`.
- Final water GPU/CPU gate passed: 26 GPU proofs, 75 renderer tests, 2 preset tests; `/tmp/water-final-gpu-gate.log`.
- App water tests (2) and app build passed: `/tmp/water-final-app-tests.log`, `/tmp/water-final-build.log`.
- Use `env RUSTC_WRAPPER=` for Cargo: configured sccache failed in this session. Metal tests require execution outside the sandbox; sandbox-only runs failed to discover the device. Do not reinterpret those environmental failures as shader failures.

Still open:
- BUG-01vr: fresh numerical comparison at 0.5 s gives 7.459 mm coarse/fine vs 10.67 mm fine/finer mean particle-position difference (ratio 0.70). Pool 2 s max/mean deltas 147.5/41.93 mm; max relative density delta 0.2368. Existing diagnostic tests enforce finite/bounded/no-fault state but leave timestep agreement unasserted. No formula or acceptance threshold was relaxed. Do not call the physics validated based on these passing tests. `/tmp/water-numerical-metrics.log`; updated bead comment records lead decision and evidence.
- Integration coverage is incomplete: actual executor-backed paused impulse/resume, fault after emission/impulse, export abort, and asynchronous reset/fault lifecycle are not all covered. New high-index/tail GPU tests directly dispatch kernels, not their host primitives. One CPU helper test checks clean-pose ticket ordering; it does not test Metal completion/reset lifecycle.
- Final-submission fault visibility in export still needs an end-to-end check: live reporting polls prior completed status, so a passing export-overload test alone cannot establish GPU fault detection on the last frame.
- Performance target (BUG-kjxf), water shadows (BUG-v9gv), and full ten-second default-sequence acceptance remain unverified. Two-way rigid-body coupling/buoyancy and actual ray-traced water remain outside the implemented feature; the cube prescribes motion and pushes water.

Original detailed review: `/tmp/manifold-water-review-3ed378131.md`; durable findings/status are on BUG-vglg and BUG-01vr. Preserve unrelated main-checkout `.beads` changes. All app edits belong to slot-7. No main merge or landing gate has been performed.

Launch the rebuilt slot from its working directory: `cd '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-7' && ./target/debug/manifold`. Recreate/reselect Water — Prototype to use the updated bundled graph if an existing project retained its old instance.
