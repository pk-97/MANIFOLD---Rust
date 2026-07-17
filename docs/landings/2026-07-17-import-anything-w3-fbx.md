# Import-Anything Lane W3 — FBX/.obj/.dae drag-and-drop via Blender — landing report

Directive: `docs/IMPORT_ANYTHING_WAVE_DESIGN.md` Lane W3 (part of the overnight
Import-Anything wave). MANIFOLD stays glTF-only internally; FBX/.obj/.dae drops now
convert through the user's installed Blender as a subprocess before flowing through
the existing glTF import path.

## What landed

- `crates/manifold-app/src/blender_import.rs` (new): Blender discovery (UserPrefs
  `MANIFOLD_BlenderPath` override → bundled macOS app → `which blender`), a
  timeout-guarded (120s) subprocess conversion via `scripts/blender/fbx2glb.py` with
  stderr-tail capture on failure, and a best-effort `--version` capture for the report
  line ("converted from FBX via Blender 4.5.2").
- `crates/manifold-app/src/app_lifecycle.rs`: `import_model_file` converts convertible
  extensions to `.glb` first — one function seam ahead of the existing blocking
  `assemble_import_graph` call, same shape the function already used for its blocking
  CPU parse. No UI-thread restructuring needed (the STOP clause did not trigger).
- `crates/manifold-app/src/app.rs`: drop-dispatch routes `.fbx`/`.obj`/`.dae` into the
  same branch as `.glb`/`.gltf`.
- `crates/manifold-app/src/user_prefs.rs`: `app_data_dir()` helper (the directory
  convention other subsystems should reuse) + a `cfg(test)` in-memory constructor so
  the new module's tests don't need the `ui-snapshot` feature.
- Converted models cache to `<Application Support>/MANIFOLD/converted_models/<stem>.glb`.

## Tests

- Unit tests for Blender discovery order (fake/stand-in paths, no real Blender
  required): pref-path precedence, fallthrough when the pref path is stale, and the
  bundled/`which` fallthrough chain.
- `real_conversion_produces_a_skeleton_posed_import`: env-gated
  (`MANIFOLD_RUN_BLENDER_TESTS=1`) `#[ignore]` integration test — generates a rigged
  FBX via `scripts/blender/make_hostile_rig.py` into a temp dir (never committed),
  converts it through the real `convert_via_blender` path, imports the produced glb,
  and asserts `node.gltf_skeleton_pose` drives the mesh. Verified passing on this dev
  machine (Blender 4.5.2 LTS).

## Incidental fix picked up during landing

Merging `origin/main` (three times, as sibling lanes kept landing) surfaced a stale
`docs/node_catalog.json` — `node.ssao_from_scene_depth` had gained `projection`/`relief`
params on main without a catalog regen, so `catalog_gen::tests::regenerates_in_sync`
went red. Unrelated to W3; fixed via `cargo run -p manifold-renderer --bin gen_node_catalog`
in the same landing so the gate stayed green (commit `aad366df`).

## Gate history note

Two early conformance-sweep runs failed on transient GPU OOM (`kIOGPUCommandBufferCallbackErrorOutOfMemory`)
under confirmed cross-session GPU contention (another lane running the identical sweep
concurrently, verified via `ps aux`). A third run, in a clean window, hit a different,
deterministic failure (`Graph::add_node_named: duplicate handle 'mat_0/mat_0'` on
`MetalRoughSpheresNoTextures.glb`) — diagnosed by running the same sweep against a clean
copy of the then-current `origin/main` tip, which passed, proving the bug was already
fixed upstream between the main state this branch had merged and its current tip. Merging
the newer main resolved it; no code changes to `manifold-renderer` were needed for W3 itself.

## Gates run (all green at landing)

- `cargo test -p manifold-renderer --lib`
- `cargo clippy -p manifold-renderer --features gpu-proofs --tests -- -D warnings`
- `cargo clippy -p manifold-app -- -D warnings`
- `cargo nextest run --workspace`
- `cargo test -p manifold-renderer --lib --features gpu-proofs hostile`
- `MANIFOLD_RUN_BLENDER_TESTS=1 cargo test -p manifold-app --bin manifold -- --ignored real_conversion_produces_a_skeleton_posed_import`
- `cargo test -p manifold-renderer --features gpu-proofs --test glb_conformance`

## Backlog

No BUG-186/backlog row is tied to W3 per the wave design doc; `docs/BUG_BACKLOG.md`
has no open FBX/Blender-specific entry to flip.
