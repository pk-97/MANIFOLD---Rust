# Box3D demo — implemented, not landed

Branch: `codex/box3d-demo`. Implementation commits: `f58e2aa97`, `1989b77db`. Current merged tip: `10c1812e7`; base main: `e1ded61e8`.

The Physics Solids preset has all five Platonic solids falling onto a fixed floor through a pinned Box3D C library and a thin owned Rust wrapper. Existing Scene controls expose body mass/friction/bounce and World gravity/speed/Reset. Existing materials and rendering are reused. The material inspector work in the other task was left intact. Current architecture and limitations are in docs/BOX3D_PHYSICS_DESIGN.md.

## Verification

Focused wrapper, simulation, geometry, preset wiring, exposure/serialization, UI routing and actual Metal scene proofs passed. The Scene UI flow passed, including speed undo and Reset. Visually inspected initial and fallen renders and the Scene controls. Final landing gate: tooling, feature coverage, design status, UI flow, dependency checks, ignored-test audit, clippy and regular tests passed. GPU suite: all integrations passed and no golden drift; 2475 renderer library tests passed and one existing timing-budget test failed.

Sole remaining failure: `preset_runtime::layer_skin_tests::mutual_skin_two_layers_render_300_frames`, max 23.83 ms against 20 ms, average 0.52 ms. This test passed during the first full run of this demo. The existing issues BUG-7gu8 and BUG-ovnb describe the same intermittent timing failure. Comment added to BUG-7gu8 with this evidence. No thresholds changed. Two full landing attempts were made; stopped per repository attempt limit. The first run's catalog, thumbnail and shader ABI registration omissions were fixed in 1989b77db and all passed in the second run.

Local evidence survives retirement in /tmp/box3d-final-landing-gate.log, /tmp/box3d-final-gpu-proofs.log, /tmp/box3d-landing2.log, /tmp/box3d-reset-ui.log, /tmp/physics_solids_initial.png and /tmp/physics_solids_settled.png.

## Remaining work

Review the known timing-budget red and finish landing through scripts/land_branch.py. The repository permits the lead to record a named-red verdict, but no exception landing has occurred. Main has unrelated staged/unstaged beads changes; do not sweep these into this work. Main's .beads/interactions.jsonl also contains unrelated uncommitted history, which land_branch.py would commit wholesale if using its named-red path; preserve that work.

Known demo limitation is tracked as BUG-g3c3: generic Scene duplicate/remove needs physics-aware handling for the shared solver. This is a basic six-body demo, not a 16K-body performance qualification. Max 16 body slots, fixed 120 Hz ticks, four substeps, process-wide native-call mutex for upstream registry safety, and animated-body velocities are not yet supported.

When restored into a slot, launch with `cargo run --manifest-path "/absolute/path/to/slot/Cargo.toml" -p manifold-app --bin manifold`, then choose Physics Solids and open Scene. No outstanding worker changes. Archive preserves all source; archive status is not app landing approval.
