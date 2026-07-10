# Scene Build + Group Params — P1 landing

**Landed:** 2026-07-10 · main `3a6e30b7` (merge of `feat/scene-build-p1`, worktree tip `430ad571`)
**Phase:** P1 — `PortType::Transform` + `node.transform_3d` atom · **Level reached: L1** (unit-tested; nothing consumes the port yet — P2 is the vertical slice)
**Orchestrator:** Opus · **Worker:** Sonnet

## What shipped

- `crates/manifold-renderer/src/node_graph/transform.rs` — `Transform { pos, rot_euler (radians), scale }` CPU-struct, `Default` = identity (pos 0 / rot 0 / scale 1), transcribed verbatim from design §3. Wired into `node_graph/mod.rs`.
- `PortType::Transform` variant + full plumbing across `ports.rs`, `backend.rs` (+ `MockBackend`), `bindings.rs` (`NodeInputs::transform`, `NodeOutputs::set_transform` + `pending_transform_writes`), `execution.rs` (scratch + drain), `metal_backend.rs`, `primitive.rs` macro arm, `snapshot.rs`, `catalog_gen.rs`.
- **UI mirror-enum boundary** (not on the original checklist; caught by the compiler): `manifold-ui/src/graph_view.rs` mirror `PortKindSnapshot` + `manifold-app/src/ui_translate.rs::port_kind_to_ui` both gained a `Transform` arm — the sanctioned translation boundary per the ui-foundation convention.
- Editor pin color: `PORT_TRANSFORM_COLOR = Color32::new(255, 128, 199, 255)` (hot pink, hue ≈326°) in `graph_canvas/mod.rs`, consumed in `graph_canvas/model.rs`. Nearest existing hues (Camera salmon ~0°, Texture3D purple ~273°) are >45° away.
- `crates/manifold-renderer/src/node_graph/primitives/transform_3d.rs` — `node.transform_3d`: nine TRS params verbatim from `render_scene`'s current per-object generation (labels/ranges/`ParamType::Angle` radians, minus `_{i}` suffix), nine same-named optional scalar input ports (port-shadows-param), one `transform: Transform` output, full `PrimitiveDescription`.
- Unit tests (16): identity default; param→output (radians preserved); one port-override test per TRS family.

## §2.5 audit verdict (confirmed by inspection)

Port = one-wire-from-existing — fourth CPU-struct port alongside Camera/Light/Material, same accessor shape, zero GPU resource on the wire. Atom = genuinely new — `affine_transform` is a 2D UV effect (not TRS); `generate_instance_transforms`/`InstanceTransform` is the GPU instancing array path (anonymous copies, GPU Pod). No existing primitive covers TRS-as-a-named-CPU-struct-port.

## Gate (orchestrator-run, verbatim)

- `cargo test -p manifold-renderer --lib transform` → `16 passed; 0 failed`.
- `cargo clippy --workspace -- -D warnings` → clean (`Finished in 18.38s`).
- Full GPU-free workspace sweep → all green after one inherited failure fixed (below).
- `check-presets` (worker, against worktree) → `47 presets: 47 ok, 0 failed`.
- Negative: `rg Arc<Mutex|Arc<RwLock` in `transform.rs`/`transform_3d.rs` → 0 hits.
- Forbidden gate: `render_scene.rs` diff empty across the branch (untouched, as required).

## Inherited-failure note

The concurrent landing `807b6a91` (harness-fidelity proposal) added `docs/HARNESS_FIDELITY_INVARIANT_PROPOSAL.md` without regenerating `docs/README.md`, leaving `manifold-core`'s `docs_index_sync` drift-guard red on origin/main. Regenerated the index in-branch (`430ad571`) so P1 lands on a green tree rather than on top of the red one. Index diff was the one new entry + count bump + three drifted description refreshes; no entries removed.

## Shortcuts taken

None. Every plumbing site found by grep and confirmed by the compiler, including the unlisted UI mirror pair.

## Demo artifact

None — L1 phase, `render_scene` untouched, nothing renders yet.
