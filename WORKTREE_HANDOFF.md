## Paused — user-requested look checkpoints, 2026-09-11

Peter stopped further simulation work because of cost and visual regressions.
Do not resume fixes, tests or renders without a new request.
Current complete WIP source is preserved by tag `codex/water-checkpoints/2026-09-11/current-wip`.
This is an archival commit, not a verified app landing. No merge to main.
Existing failed visual/physics/optics acceptance remains failed.
No new build, test or render was run for checkpoint creation.

Preserved videos, projects, graphs, earlier images/configs/logs and HDRI:
/Users/peterkiemann/.codex/visualizations/2026/09/11/01a08ef4-0edc-7cc2-bc55-8ce1b245710c/water-apic/checkpoints-2026-09-11
Open index.html for the gallery; README.md explains restore limitations.
Intermediate APIC looks lacked source commits: preview/settings archives alone
do not guarantee exact executable restoration. Four earlier committed source
stages also have named tags under `codex/water-checkpoints/2026-09-11/`.
Source manifest and Git bundle are stored with the archive.

Prior experiment handoff follows.

## Current connected surface and native water RT preview — 2026-09-11

Peter explicitly chose KEEP THE DAM-BREAK RELEASE. Seed/solver were not changed:
1.5 × 0.625 × 1.75 m block, zero initial velocity, APIC h=.0625 m, spacing=.03125.
Opening-frame inspection shows the prior Yu–Turk field reveals isolated particles
hidden by the old reconstruction. This is an appearance explanation, not an
energy/trajectory proof of the violent startup. BUG-01vr physics refinement is RED.

Implemented three components operations (seed, atomic union, roots), nominal
.03125 m connectivity and 2013 Eq17 fit filtering. SPH rho remains unfiltered.
Fit binding4 is components; shapes moved to5. Main/registry/prewarm/proof modules
are wired. Inclusive radius, inactive slots, shuffled801-chain, separate sheets
and bridging cases pass independent CPU BFS/native GPU checks. Fixed Naga's
required total return paths without imposing a relaxation-pass cap.

Native water_raytrace.metal is appended to the prewarmed RT library. The new
128B WaterRayParams and fixed76-entry binding array reuse TLAS resource lifetime
and bounded96-query regions. render_scene now supports RT water with matching
water_density/isovalue; graph node58 shares the isovalue with primary extraction.
Reflection, Snell entry/exit, Beer path length, first-Sun visibility and four
internal dielectric continuations feed the existing depth-tested water pass.
Code is experimental: entry bracket precision mismatch and later liquid intervals
remain open as BUG-vglg.1 and BUG-vglg.2. No self-shadowing or caustics.

PASS: focused GPU+renderer clippy --tests --features gpu-proofs -Dwarnings;
app/renderer/exporter/graph-tool build; two graph validations and project loader;
14 native proof cases across the initial/fixed focused gates plus two component
unit tests; git diff --check. Gate logs /tmp/water-components-rt-proofs.log and
/tmp/water-components-rt-proofs-fixed.log. First gate caught invalid WGSL return
paths and f64 fixture normals; corrected cases all pass. Build initially failed
on a worker workgroup type and a lead edit that deleted a renderer block. Higher
effort review restored the exact HEAD block; clippy/proofs/subsequent build pass.
Graph-tool needs escalated Metal device access; sandbox attempts found no device.

Bounded visual work is complete for this pass: 180 frames each at128³/256³ with
identical component fit and raster lighting, then900 frames with256³+RT.
Observed matched frames30/90 and RT480: finer grid smooths edges but does NOT fix
round bulk/droplets; RT adds scene reflections/shadows and severe dark stippling.
Visual acceptance FAILS. Stop further render sweeps; a focused curved/perturbed
entry value proof is needed before another fix. The reviewed static defects do
not yet isolate every dark pixel; Phong/point-fill secondary-hit parity is also
unestablished. Do not present the preview as finished water or SOTA.

Timings encode+submit+GPU wait, excluding PNG, NOTnative-appFPS:128³ raster107.155ms,
256³ raster196.076ms (frames60..179),256³ RT244.028ms (frames60..899).
Captures /tmp/water-components-{128,256,rt}; no logged runtime fault.
Artifact root: /Users/peterkiemann/.codex/visualizations/2026/09/11/01a08ef4-0edc-7cc2-bc55-8ce1b245710c/water-apic
wave-obstacle-apic-components-rt-30s.mp4 is1920×1080,900frames/30s;
wave-obstacle-surface-128-vs-256-6s.mp4 is1920×540,180frames/6s (128LEFT/256RIGHT).
Wave Obstacle - APIC Water RT.manifold loads; native app knobs not exercised.
Launch from owned slot7: ./target/debug/manifold; Cmd+O project, Space.
Do not close an unsaved existing app instance blindly.

At the time of that preview, changes were uncommitted in wave/live-water.
The subsequent user-requested WIP preservation checkpoint is recorded above.
Main landing remains blocked while visual/optics/physics gates are open. The graph depends on
the existing uncommitted MAC integration; do not carve it into a falsely standalone
verified reconstruction landing. Prior Yu–Turk baseline video/project remain in
the artifact root and /tmp/water-apic-yu-turk*.

Previous stage context follows.

## Current reconstruction comparison — 2026-09-11

Peter authorized improved reconstruction with the current solver and scene fixed.
Extended water_density_field with optional bounded covariance shapes from the
existing water_surface_fit. Density radius remains 0.10m equivalent sphere
support, fitted axes normalized by their determinant; center blend0.5. Search
covers 2.52*radius+.125m from original bins so stretched/shifted support is not
clipped. Shape channel names added to the well-known registry. No solver changes.
Repo APIC wave fixture adds one fitting node/four wires only. Daylight comparison
graph audited equal to baseline outside those additions; same camera/material.

Checks PASS: generated WGSL validation (one missing-channel-name error fixed),
focused renderer clippy tests+gpu-proofs, required GPU gate with four actual
tests (spherical density, rotated-plane fit, new fitted density f64 oracle,
absent-shape parity), graph validation and app project loader. New proof tests
check rotated and shifted support beyond old bins, weighted foam and mapped
GPU input byte immutability. Logs /tmp/water-reconstruction-*.log. No repeated
old optics/physical acceptance checks. The earlier water_scene occlusion ratio
failure and BUG-01vr dt-refinement acceptance remain open: no main landing.

One900frame1080p30 verification completed without solver fault. PNGs
/tmp/water-apic-reconstruction; timing /tmp/water-apic-reconstruction-timing.json:
mean108.163ms, median102.303, p95146.681, frames60..899,
encode+submit+GPU wait only (not native appFPS). Baseline mean75.823ms.
Observed matching frames90 and480: sharper/thinner folds, still broad gel-like
surface. Modest visual improvement and higher cost; not final water acceptance.
Do not run more appearance sweeps without new user scope.

Artifacts in /Users/peterkiemann/.codex/visualizations/2026/09/11/01a08ef4-0edc-7cc2-bc55-8ce1b245710c/water-apic:
wave-obstacle-apic-reconstruction-30s.mp4 (full1080p),
wave-obstacle-reconstruction-comparison-30s.mp4 (old left/new right),
WaterWaveTankApicReconstruction.json,
Wave Obstacle - New APIC Reconstruction.manifold (existing playground controls
plus Surface Center Smoothing). Project and embedded graph copies agree.
Native control interaction remains untested; existing running app not closed.
Latest worktree binary built: cd "/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-7" && ./target/debug/manifold.
Save/quit old instance first, launch normally, Cmd+O project, Space to play.

Earlier context follows; reconstruction-unchanged statements below describe
the preceding optics-only pass.

## Current APIC appearance result — 2026-09-11

Authoritative worktree slot-7, branch wave/live-water, owner codex-water-research-01a08ef4. The auto checkout is not this workstream. Preserve all uncommitted native MAC APIC and offline work. Native stage proofs passed earlier; whole-scene physical/refinement acceptance remains open under BUG-01vr. No main landing; raw feature push is prohibited by the repo landing-only policy.

Latest user: new physics looks much better but fluid does not read as water; continue material calibration. Peter explicitly says WIP water needs NO legacy compatibility. Removed RenderScene's 1/10.9 additive-splat correction: water thickness is now metres directly, old uniform field is padding, no units enum. APIC physics group and density reconstruction are unchanged (verified exact JSON equality to prior .manifold project). Fixture foam defaults0, studio strips0, absorption exp(-[.340,.0565,.00922]) at1m from corrected NASA Pope/Fry650/550/450nm (documented RGB approximation), IOR1.333 unchanged.

Observations: six-second no-foam/no-absorption control /tmp/water-apic-clear-diagnostic revealed strip-light contours remain; the metre correction+clearwater+stripoff30s capture /tmp/water-apic-clear-final removed coating/contours but was visually flat. One subsequent daylight comparison reuses existing node.hdri_source and existing kloppenheim_07_puresky_4k.exr, same physics/optics/camera, /tmp/water-apic-daylight.json. Early/late observed frames show subtle sky reflections but smooth/sheet-like water persists. Do not claim convincing final water, paper-level full physics acceptance, full RT, or live30FPS. Stop further appearance/test sweeps this pass.

Both900frame1080p30captures complete without runtime solver fault. Daylight timing mean75.823ms, median75.360ms, p9580.213ms, frames60..899, encode/submit/GPU completion only, excludes PNG readback/encoding/writes. NOT native-appFPS. Final artifact path: /Users/peterkiemann/.codex/visualizations/2026/09/11/01a08ef4-0edc-7cc2-bc55-8ce1b245710c/water-apic/wave-obstacle-apic-daylight-30s.mp4. Matching project Wave Obstacle - New APIC Daylight.manifold and graph WaterWaveTankApicDaylight.json live beside it. The repo APIC fixture remains the neutral clearwater control; daylight is project-scoped. Daylight graph/project validate, /tmp/water-daylight-graph-validation.log and /tmp/water-daylight-project-validation.log.

Validation: /tmp/water-optics-clippy.log PASS (--tests --features gpu-proofs), /tmp/water-optics-build.log PASS (app/capture/graph-tool), /tmp/water-optics-graph-validation.log PASS. Native GPU gate /tmp/water-optics-gpu-gate.log remains RED:5pass/1fail. The1m actual-graph Beer-Lambert/Fresnel proof passes, as do GGX, no-watergolden, unsupported combos, artifact. water_scene_occlusion_and_depth fails r/b at(41,78)=.995578 vs<=.995. Existing occlusion fixture uses known.12m thickness; no tolerance relaxed. Do not claim green gate or land. Earlier two failures were private imports and testhelper writing unconnected textureoutputs; both fixed. Current failure preserved in BUG-01vr comment and gate log; stop further speculative fixture changes. All worker ownership returned.

Native app has NOT opened new project: existing MANIFOLD instance rejected the built binary. Do not close unsaved work blindly. CUA app selection previously stalled badly. Build launch executable is '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-7/target/debug/manifold'; active water overlay disables full-sceneRT. Next substantive work is surface/scene-reflection quality and precise diagnosis of the edge-pixel test, not another magic water-parameter preset.

Guard note: active hook script comes from main git-common-dir. Desktop reports auto-checkoutcwd for some calls; necessary exact-command permits must use main .codex/hooks/guard.py and reportedcwd /Users/peterkiemann/.codex/worktrees/5034/MANIFOLD - Rust, while actualexec and explicitmanifest stay slot-7. Wrong-root/slotcwd permits remainedunconsumed; no guards were modified or bypassed.

Earlier checkpoint below is historical and superseded for liveintegration status.

---

## APIC replacement checkpoint — 2026-09-11 (current)

Continue in slot-7 on wave/live-water, owner codex-water-research-01a08ef4. Do not acquire a new slot or use the Codex auto-checkout. Verified CPU transfer/pressure references and the revised contract are committed locally at 080f75ee8 (base bf9e56965). Push was blocked by the repository PreToolUse hook: raw feature-branch push is forbidden and land_branch.py has no checkpoint-only mode. Do not land main while BUG-01vr remains open.

The live app solver and renderer are unchanged. The offline render bridge now includes tests/water_apic_coupled_reference.rs (experimental coupled acceptance, still red), its tests/support/water_offline_export.rs shim, and the water-offline-render binary with density-mesh helper. Untracked GPU drafts to preserve: primitives/mac_{scatter_mass_momentum,resolve,extrapolate}.rs and their three shaders, and tests/gpu_proofs/water_mac_atoms.rs. GPU drafts are deliberately unregistered after native compilation failed. Re-integration requires the three pub mod declarations, MAC_VELOCITY/MAC_VALID channel-name entries, GPU proof module, and explicit startup prewarm for the hand scatter; generated stages use the registry codegen sweep. This branch's primitive! macro does NOT support the install hook shown in stale ADDING_PRIMITIVES instructions. Source strings are prepared in constructor extra_fields; runtime retrieves the prewarmed pipeline cache.

Physics evidence and unresolved acceptance are in docs/WATER_SIMULATION_DESIGN.md and the latest BUG-01vr comment. The final static-box sampling correction is even tangential / odd normal reflection; moving-object sampling and a full GPU pressure/advection pipeline are not implemented. A 30-second small-wave validation video is now rendered; it has no central object/breakwater and is not the original wave-tank scene. Stop further speculative solver changes: monotonic timestep-refinement acceptance failed before and after the boundary correction. Compare the complete pinned upstream engine on the same fixture before proposing another correction. The physical probe keeps its failure assertions intact.

Checks: focused renderer clippy passed; feature-gated GPU-proof clippy passed; eight generated WGSL/ABI unit checks and four transfer-oracle tests passed within the scoped gate. Native execution failed before dispatch in Naga 28 SPIR-V emission, block.rs:3369, 'Expression [97] is not cached'; all three proofs construct scatter first, so isolate that source before attributing the panic to extrapolation. Two native-gate attempts were used (first fixture compile error, then translator panic). Initial cargo also hit sccache EPERM; checks used per-command RUSTC_WRAPPER= with no configuration change.

Logs: /tmp/water-slip-wave.log (final 251-second CPU run), /tmp/water-resolved-wave.log (pre-reflection), /tmp/water-mac-gpu-gate.log, /tmp/water-mac-clippy.log, /tmp/water-gpu-clippy.log. Manual probe: rustc --edition=2024 --test -O crates/manifold-renderer/tests/water_apic_coupled_reference.rs -o /tmp/water-slip-wave; /tmp/water-slip-wave --exact coupled_resolved_wave_acceptance --nocapture. Do not repeat without new evidence/authorization. Native gate used RUSTC_WRAPPER= python3 scripts/gpu_proofs_gate.py --manifest-path '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-7/Cargo.toml' --filter water_mac_atoms --filter node_graph::primitives::mac_. No new live app build exists for this solver. Offline launch command: `'/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-7/target/debug/water-offline-render' /tmp/water-apic-cache /tmp/water-apic-rt-rerender 900 1920 1080`. This requires the completed cache; do not rerender without a new reason.

Rendering continuation evidence: `/Users/peterkiemann/.codex/visualizations/2026/09/11/01a08ef4-0edc-7cc2-bc55-8ce1b245710c/water-apic/apic-small-wave-manifold-rt-30s.mp4` is verified 1920×1080, 30 FPS, 900 frames, 30 seconds. All 900 frames logged eight actual RT capture ticks; first/last images inspected and no ERROR entries. `/tmp/water-apic-cache` contains the uninterrupted CPU reference sequence, 92160 particles per frame. This is MANIFOLD's production Graph/Executor/scene_object/PBR/Metal renderer reached through a custom offline cache-to-mesh bridge. It is not live transport/interaction integration. Blend water uses environment GGX and screen-space transmission and is excluded from RT acceleration; RT lights the opaque scene. Do not call this ray-traced water or a live performance result. The original central-object wave demo remains unported.

Render validation: renderer clippy --tests passed; three bin tests and two exporter tests passed. The focused gpu_proofs_gate filter `rt_bug88m_blend_specular_gate` passed its one native proof. Logs: `/tmp/water-final-clippy.log`, `/tmp/water-offline-tests.log`, `/tmp/water-export-tests.log`, `/tmp/water-offline-rt-gate.log`, `/tmp/water-apic-rt-sequence.log`. Existing solver acceptance failures were not retuned or rerun. BUG-01vr has the render evidence and unfinished integration/RT scope.

Earlier handoff history follows; its implementation/model/usage instructions may be stale.

---

## Wave tank trial — 2026-09-11

User requested a classic asymmetric wave scene. Added a NON-BUNDLED reproduction
fixture `crates/manifold-renderer/tests/fixtures/presets/WaterWaveTank.json`:
flat 3.3×2.25 m basin, stationary offset breakwater with matching collision
half-extents [.12,.35,.30], fixed camera, no pour or cube motion. The initial
water reservoir at one end releases under gravity. A sloped bed/rock collider
is outside the current box-only collision path.

Both 240-frame attempts failed the existing kinematics guard (0x8): .5 m
release height at frame67 (~1.12s); corrected geometry and .3125 m release
height at frame154 (~2.57s). No solver constants or safety limits changed.
The fixture stores the second attempt; it is excluded from the bundled preset
catalog. No app rebuild or main landing for this failed trial. Stop further
parameter trials; diagnose this reproducible solver failure (BUG-01vr).
Observed filmstrip `/tmp/water-wave-tank-trial-filmstrip.png`, 154-frame preview
`/tmp/water-wave-tank-trial.mp4`, fault log `/tmp/water-wave-tank-final.log`.
Reproduce using the existing release `render-generator-preset WaterWaveTank`
with `--preset-file` pointing to this fixture and `--size 1280x720 --frames 240`.

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
