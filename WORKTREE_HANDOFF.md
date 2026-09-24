# Standard Box3D object physics

Active local slot-0, branch `codex/flower-mesh-drop`, retained for Peter to test and iterate. No push or app landing. Tracking: BUG-vaj5.

The custom dynamic mesh collision experiment is removed. Vendored Box3D collision/solver sources match the original pinned engine. Imported scan rendering stays original; collision uses fitted standard convex hulls, with a 0.1% shell for open surfaces. This is approximate collision geometry, not exact triangle contact.

The existing Rigid Body and Physics World nodes now support prepared imported geometry and 64 body slots. Hulls prepare during asset warmup; the shared world holds until inputs are ready. Existing Fixed/Dynamic/Animated motion, mass, friction, bounce, gravity, speed, reset and transport stepping remain authoritative.

Scene actions enable/disable physics and split an imported rigid object into eight pieces. These are ordinary bodies in the shared world. Splitting is an authoring edit, not impact-triggered runtime fracture. It preserves source triangles/materials but adds no cut caps. Unsupported deformed or skinned sources reject the action.

Original tiger lily: 454,840 triangles. Optimized production-runtime CPU benchmark, six seconds at 60 Hz:

- Intact, 32 fitted hulls: 0.006 s stepping; 0.016 ms mean, 0.029 ms p95, 0.073 ms maximum; 0.688 s preparation.
- 32 independent pieces: 0.081 s stepping; 0.224 ms mean, 1.042 ms p95, 12.777 ms maximum; 7.251 s sequential benchmark preparation.

These measurements exclude rendering and do not qualify thousands of pieces. The original source binary remains unchanged. The earlier four-minute video is from the removed experiment and does not represent this implementation.

Focused native/runtime tests and the imported-flower Metal proof exercise standard hulls, fixed-tick timing/reset, triangle preservation, production scene commands, save/reload, loading and rendered motion. The imported-object UI flow passes enable, split, disable, undo and shared World controls. Focused clippy passes; existing AVFoundation deprecation warnings remain. No main landing gate or whole-show performance qualification is claimed.

Benchmark:

```sh
cd '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-0'
env RUSTC_WRAPPER= bash .claude/scripts/with-build-lock.sh cargo run --profile test --manifest-path "$PWD/Cargo.toml" -p manifold-renderer --example physics_mesh_benchmark -- '/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/gltf/cc0__tiger_lily.glb' 32
```

Local test projects (ignored, retained with the active worktree):

- `tests/fixtures/standard-box3d-intact.manifold`
- `tests/fixtures/standard-box3d-split.manifold`

Launch the worktree app, then open either project:

```sh
cd '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-0'
./target/debug/manifold
```

Use Scene → select an imported rigid object → Enable Physics. Split into 8
creates independently editable pieces. Source selection and materials stay
with each piece. The two project files contain the original tiger lily and a
large fixed floor. `cargo build --profile test -p manifold-app --bin manifold`
produces the optimized local executable at the launch path above.
