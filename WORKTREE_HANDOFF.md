## Surface reconstruction continuation — 2026-09-11

Slot-7 `wave/live-water`: added neighbour covariance ellipsoids shared by depth,
thickness and foam, wired into both water demos. Particle simulation is unchanged.
Observed 240-frame 720p WaterImpact sequence: substantially less particle lattice;
water still looks too smooth and lacks convincing localized foam/spray. Keep as
an incremental reconstruction improvement, not reference-image acceptance.
Tighter projected bounds reduced median frame cost from 24.942 to 22.607 ms
(p95 23.460); prior spherical baseline was 18.312 ms. Headless encode/submit/GPU
wait only, not native app FPS. Final filmstrip `/tmp/water-fitted-final-contact.png`,
frame 90 `/tmp/water-fitted-final-seq/frame_000090.png`, timing
`/tmp/water-fitted-final-timing.json`. Fixed camera retained.

Native ellipsoid depth/oriented-plane fitting and legacy sphere depth/thickness/
foam checks passed. Focused clippy, both preset roundtrip tests and release app build passed:
`/tmp/water-shape-roundtrip.log`, `/tmp/water-shape-app-build.log`.
Daily shared weekly meter last read 12%,
baseline 8%, closeout 12%, finish 13%. BUG-01vr numerical acceptance remains
open: no production pressure replacement and no main landing. Next work is
solver validation/integration, followed by motion-localized whitewater and spray.

## Current day pass — 2026-09-11

Implemented persistent deformation-driven foam, near-surface coverage and optional water shading; WaterPrototype and new WaterImpact expose gain/lifetime. This is a visual proxy, not physical bubbles or spray. Rebuilt release app. Native foam/water compatibility checks (6), CPU roundtrip/MAC reference (7), and focused clippy pass. Kernel recurrence checks do not establish host reset/epoch lifecycle coverage. Final impact movie `/tmp/water-impact-foam.mp4`; 240-frame 720p timing `/tmp/water-impact-accepted-timing.json`: median18.312ms, p9519.248ms (encoding/submit/GPU wait, excluding PNG). Appearance remains short of reference: rounded sheets, visible particle sampling, no thin airborne spray. Camera stays stationary. No main landing: BUG-01vr is open.

Test-only MAC scatter/resolve/gather now passes with operator-specific checks: scatter against independent f64, gather against independent f64 supplied the actual quantized grid. Prior full-roundtrip low-mass discrepancy remains a reported diagnostic. No production pressure replacement. Day usage baseline8%, last live10%; shared rounded meter. Historical details below.

# Water review fixes — 2026-09-10

## Completed performance pass; physics acceptance open

Peter authorized implementation on 2026-09-10. Starting account-wide weekly usage: 3%. Target approximately +5 percentage points; begin closeout at 9% used (+6), finish by 10% used (+7), emergency stop at 11% used (+8). Recheck at lane boundaries; readings are shared and lagged, not a hard enforcement mechanism. One Astra lead for decisions/review/visual acceptance only; bounded Luna low workers, maximum two active, GPU captures and timings serialized. Initial workers: night_capture (existing headless tool upgrades and 720p/10-second baseline) and night_numerics (read-only production-vs-proof parameter and clock audit). Initial briefs: /tmp/water-night-capture-brief.md and /tmp/water-night-numerics-brief.md. No unattended main landing while BUG-01vr remains unresolved.

Target: 60 FPS at1280x720 for one water layer on this Mac, honest real-time simulation progression, no faults over ten-second default pour/impact, smoother surfaces and believable jet/droplets. Foam/spray are conditional on motion and performance passing; the motion gate did not pass, so no foam/spray was added. Reference images supplied by Peter establish translucent thin edges, varied wave scales and localized whitewater; their beach geometry is outside this pass. Preserve exact initial settings for comparisons. Stop speculative retries; save concrete partial progress if budget or evidence blocks further work.

Branch `wave/live-water`, slot-7. Fixes start from `3ed37813198d6c4adbc74d20806ab8939731f708`. Do not land this feature on main while numerical acceptance remains open (BUG-01vr). Peter requested a full review, removal of camera animation, an active interaction demo, and economical Luna implementation with one lead.

Implemented: stationary camera; repeat pour with downward birth velocity; a gentle repeated cube dip; visible tank aligned to solver bounds; valid small floor meshes; camera-space refraction; seed/count consistency for the bundled preset; full stress-tail preservation; separate emitter/impulse candidate buffers; basin position correction; rejected unsupported collider motion; in-place substep CPU state updates; paused trigger observation outside zero-tick repeat bodies; asynchronous GPU fault reporting and clock freeze; explicit export overload failure; active-frame filtering of cached fatal errors; RT requests use raster water with a transition warning. Both preset descriptions reflect the new behavior.

Active pass updates: scatter accumulation now uses native `atomicAdd` with returned-old-value signed overflow detection. Faulted scratch grids are invalid and status-gated commit preserves accepted particles; positive, negative high-contention, clean-control, transfer-parity, and full water GPU proofs passed. Surface smoothing remains at step 2. The accepted cutaway hides only camera-facing Wall S and Wall W while retaining colliders. Corrected artifacts are `/tmp/water-night-cutaway-corrected-*`, with filmstrip `/tmp/water-night-cutaway-corrected-filmstrip.png` and movie `/tmp/water-night-cutaway-corrected.mp4`. Capture tooling now supports `--preset-file`, capture stride, compact contact sheets with frame/time sidecars, and wall timing for every frame >=60. Latest account-wide usage is 8% versus the 3% baseline: approximately five weekly percentage points spent, below the seven-point upper limit.

The accepted water specular-AA candidate is a bounded normal-derivative roughness filter in `water_surface_pass.wgsl`. It affects reflection mip choice and the existing direct-sun Blinn approximation; flat normals preserve the original roughness and lobe scale exactly. The first unnormalized attempt was rejected after producing brighter grid-like highlights. The corrected candidate normalizes the widened lobe, then passed the real water-scene proof and one 180-frame verification: median 15.275 ms / p95 15.550 ms over frames 60..179. This short run is not directly comparable to the 10-second timing. Final shader SHA-256: `2aa09ca723934df41ceef4b82161ccbdb0d076a26d1ac8861ee8858c2d0de781`.

Final-frame fault fix: WaterState now schedules readback after the final substep's status/collider capture (immediately for zero-tick frames). The existing runtime fatal query also checks completed, current-generation readbacks without waiting or consuming ring slots. Export queries again after its existing GPU wait and before encoding. The native executor proof injects a fault only in the second of two substeps, detects it without another render, and passes clean two-substep and completed-fault epoch-reset controls. The initial scoped GPU gate passed; the final expanded proof passed in `/tmp/water-continuation-export-final.log`. This verifies the real runtime query seam; it is not a full media-encoder export test.

Faulted collider restoration intentionally uses the last verified frame pose when asynchronous status completes. It is not atomic with same-substep GPU particle acceptance. Ring slots retain generation/ticket ownership until completion. No live GPU waits were introduced.

Validation evidence:
- Continuation: focused renderer/app clippy passed; four WaterState tests, two app GPU-completion tests, all five projection tests, and the final native export fault/clean/reset proof passed. Logs: `/tmp/water-continuation-clippy.log`, `/tmp/water-continuation-state-tests.log`, `/tmp/water-continuation-app-tests.log`, `/tmp/water-continuation-export-final.log`. Existing AVFoundation deprecation warnings remain.
- Production capture: `/tmp/manifold-water-fixed-demo-v3/water_000000.png`, `water_000090.png`, `water_000180.png`. 181 frames at 640x480, stationary camera, visible pour/cube interaction, no reported fault. The accepted bundled preset also completed the ten-second observation with no reported fault; numerical and native-app performance acceptance remain open.
- Focused RT fallback Metal proof passed: `/tmp/water-rt-fallback-proof.log`.
- Final clippy passed: `/tmp/water-final-clippy.log`.
- Final water GPU/CPU gate passed: 26 GPU proofs, 75 renderer tests, 2 preset tests; `/tmp/water-final-gpu-gate.log`.
- App water tests (2) and app build passed: `/tmp/water-final-app-tests.log`, `/tmp/water-final-build.log`.
- Release app rebuilt successfully after the continuation changes (`/tmp/water-continuation-build.log`); current working-tree binary: `/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-7/target/release/manifold`.
- Current capture GPU gate passed: `/tmp/water-night-capture-gpu-gate.log`; GPU-proof clippy and the release build passed.
- Use `env RUSTC_WRAPPER=` for Cargo: configured sccache failed in this session. Metal tests require execution outside the sandbox; sandbox-only runs failed to discover the device. Do not reinterpret those environmental failures as shader failures.
- Capture baseline: `/tmp/manifold-water-timing.json` reports 10 seconds/600 frames at 1280x720, median 26.683 ms and p95 27.721 ms over frames 60..89, with no runtime fault. Atomic benchmark: `/tmp/water-night-atomic-timing.json` is a 1.5-second/90-frame verification, median 14.569 ms and p95 14.763 ms over frames 60..89; it is not the final 10-second acceptance. The first cutaway run used the wrong bundled camera and is rejected evidence. The corrected 10-second cutaway completed with no fault at 15.91 ms median / 16.87 ms p95 over frames 60..599; it narrowly misses the 60 FPS budget before app overhead. Bundled JSON syntax and the updated camera expectation pass.

Still open:
- BUG-01vr: fresh numerical comparison at 0.5 s gives 7.459 mm coarse/fine vs 10.67 mm fine/finer mean particle-position difference (ratio 0.70). Pool 2 s max/mean deltas 147.5/41.93 mm; max relative density delta 0.2368. Existing diagnostic tests enforce finite/bounded/no-fault state but leave timestep agreement unasserted. No formula or acceptance threshold was relaxed. Do not call the physics validated based on these passing tests. `/tmp/water-numerical-metrics.log`; updated bead comment records lead decision and evidence.
- Integration coverage is incomplete: actual executor-backed paused impulse/resume, fault after emission/impulse, export abort, and asynchronous reset/fault lifecycle are not all covered. New high-index/tail GPU tests directly dispatch kernels, not their host primitives. One CPU helper test checks clean-pose ticket ordering; it does not test Metal completion/reset lifecycle.
- The final-frame runtime fault query is now covered by a native executor proof. Full media-encoder export abort and reset while an old generation is still in flight remain end-to-end coverage gaps.
- Performance target (BUG-kjxf), water shadows (BUG-v9gv), and native-app performance acceptance remain open. Two-way rigid-body coupling/buoyancy and actual ray-traced water remain outside the implemented feature; the cube prescribes motion and pushes water. Q24 did not improve the trajectory probe and its CPU stress oracle was invalid because it retained Q20 scaling. The material-density trial improved the early probe but faulted with `0x8` at 1 s in static settling, so it was reverted; no thresholds were loosened.
- Physics first-fault evidence: step 194 (about 0.202 s), speed 4.007 m/s versus the 4 m/s bound, density 1156.8 kg/m³, affine norm 45.49 versus 64, acoustic CFL 0.325. The trial remains reverted.
- A test-only MAC pressure reference now passes CPU hydrostatic/zero tests and native N16 parity (max velocity error 2.95e-7, potential error 6.25e-8). The N64 fixture contains a 32×8×32 pool with fixed solid walls and air above. At 64 SOR pairs it fails the 1% divergence-residual limit (2.60%); 96 pairs passes (0.224%). Restricting SOR dispatches to the fluid bounding box preserves that result and reduces median operator wall time from 4.900 to 2.060 ms. Final p95/max is 4.659 ms from five samples after three warmups. This is a small operator probe, excluding particle transfers, moving boundaries, rendering, and app overhead. It is not a replacement production solver or a 60 FPS claim. All five final projection tests pass on native Metal.

Original detailed review: `/tmp/manifold-water-review-3ed378131.md`; durable findings/status are on BUG-vglg and BUG-01vr. Preserve unrelated main-checkout `.beads` changes. All app edits belong to slot-7. No main merge or landing gate has been performed.

Launch the rebuilt slot from its working directory: `cd '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-7' && ./target/release/manifold`. Recreate/reselect Water — Prototype to use the updated bundled graph if an existing project retained its old instance.
