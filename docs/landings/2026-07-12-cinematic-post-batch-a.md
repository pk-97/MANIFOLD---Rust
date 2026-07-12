# Cinematic post batch A — P0 (D7/I6), both layers — landed 2026-07-12

**Branch:** feat/cinematic-post → main · **Level reached:** L1 / target L1 (§10 — this cluster's
no-PNG rule, Peter 2026-07-12, overrides the standard's L2-demo minimum; a compiler phase has no
observable surface anyway — the proofs are the demo)
**Doc status line (quoted verbatim):** `**Status:** IN PROGRESS · 2026-07-12 · Fable 5 · **P0
SHIPPED 2026-07-12 (D7/I6, both layers,
docs/landings/2026-07-12-cinematic-post-batch-a.md) — derived uniforms are first-class on the
texture codegen path AND in fused regions. P1–P4 not built.**`

## What shipped

D7's derived-uniform amendment, both layers, per Peter's "no stopgaps" ruling — landing only one
layer and calling P0 done was explicitly rejected, so they land together:

- **Standalone layer** (`42929678`) — the texture codegen path (`generate_standalone_ext` /
  `generate_standalone` / `standalone_for_spec`'s texture branch) gained the same
  `derived_uniforms: &[&str]` threading the buffer path already had (vec3 → three f32 fields,
  bare name → f32, `name:ty` → explicit type, folded into the existing 16-byte-padded `Params`
  struct — placed right after scalar params, before table/multi-output/optional-input fields, to
  mirror the buffer path's own field order). Confirmed `Camera`-typed CPU-struct input ports
  already emit no GPU binding on a texture atom by construction (`is_texture_input` never matches
  `PortType::Camera`) — no fix needed there, just verification.
- **Fusion layer** (`38d2f0f8`) — `install.rs`'s derived-uniform name whitelist (`dt_scaled` /
  `frame_count` / `time*` only, wired from `system.generator_input`) and its unconditional vec3
  bail are deleted, replaced by `freeze/derived_uniform_registry.rs`: a new `inventory`-based
  registry (type_id → recompute fn), the same "data-driven, no closures serialized into the def"
  shape the existing `PrimitiveFactory` registry uses. A fused `node.wgsl_compute` kernel
  recomputes every member's derived-uniform values itself, every frame
  (`wgsl_compute::evaluate()`), from frame context and/or a routed `Camera` external. Two new WGSL
  markers carry this (`@camera_external`, `@derived_uniform_member` — see
  `FREEZE_COMPILER_MAP.md` §5). The time-family (`dt_scaled`/`frame_count`/`time*`) migrated onto
  the same mechanism, so the old per-name install code is gone entirely — any future derived
  uniform (fov, near/far, a `Light`) needs zero compiler changes, just a registered recompute.
  The `region.rs` classify exemption (a `Camera` wire no longer forces `Boundary` when the member
  consumes it entirely via `derived_uniforms`) was applied to **both** the texture and buffer
  classify paths — D7's text names only the texture line, but the fusion-layer worker found
  `install.rs`'s per-member recompute loop is already domain-agnostic, and a real shipped buffer
  atom (`flatten_to_camera_plane`, `fusion_kind: Pointwise` with camera-derived uniforms) was
  permanently boundary-ing without it. Judged as completing D7's one conceptual fix consistently,
  not scope creep — flagging it here per the standard's deviation-reporting rule.

No new production primitive was authored (`coc_from_depth` is P1's job). I6 is proven by a
`#[cfg(test)]`-only fixture primitive (`test_camera_pointwise_fixture.rs`, hand-implemented
outside the `primitive!` macro's inventory channels so it never appears in the real palette/
catalog) chained with a real pointwise neighbour and diffed fused vs unfused.

## Gate results (verbatim — independently re-run by the orchestrating session, not self-reported)

```
$ cargo build -p manifold-renderer
   Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.09s

$ cargo clippy -p manifold-renderer -- -D warnings
   Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.18s
   (zero warnings)

$ cargo nextest run -p manifold-renderer --lib
     Summary [6.054s] 1114 tests run: 1114 passed, 3 skipped

$ cargo test -p manifold-renderer --lib node_graph::freeze::proof::camera_derived_pointwise_atom_fuses_and_matches_unfused -- --exact
test node_graph::freeze::proof::camera_derived_pointwise_atom_fuses_and_matches_unfused ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 1432 filtered out

$ cargo test -p manifold-renderer --features gpu-proofs
(main lib+bins) test result: ok. 1412 passed; 0 failed; 21 ignored
(every other test binary in the crate — ui/shader/color/wgsl-validation suites) 0 failed across
all of them
```

I6 (`camera_derived_pointwise_atom_fuses_and_matches_unfused`) passes by name. The full existing
freeze proof suite (125 tests under `node_graph::freeze`, run as part of the 1412) and every
migrated time-family buffer atom's `gpu_tests` (project_3d, the particle-sim family, `project_4d`,
`scatter_particles`, `tube_from_path`, `revolve_curve`, `taper_mesh`, `bend_mesh`, `twist_mesh`,
`morph_mesh`, `push_along_normals`, `extrude_curve`, `flatten_to_camera_plane`,
`scatter_particles_camera`) all pass inside that same sweep — zero failures anywhere.

**Pre-existing failures check (FREEZE_COMPILER_MAP.md §10):** the DepthOfField-prewarm and
Liveschool FluidSimulation Ableton param-id fixtures named as known pre-existing failures were
searched for; the Liveschool one lives in `manifold-app`, outside this landing's crate scope
entirely, and no DepthOfField-prewarm test exists in `manifold-renderer`. Neither is touched by
this diff; the full sweep is 0 failures either way.

## Deviations from brief

- The `region.rs` classify exemption was applied to both `classify_node` (texture) and
  `classify_buffer_node` (buffer), not just the texture line D7's prose names — see "What
  shipped" above for the reasoning (a real shipped atom needed it, and the underlying loop is
  already shared).
- Array-length-family `derived_uniforms` (`weights_len`, `active_count`-from-array-size,
  `disp_w`/`disp_h`) were deliberately NOT migrated onto the new registry — D7 commits only the
  time-family + camera; those continue to fail-closed to unfused exactly as before (no
  regression, no new capability claimed).

## Shortcuts confessed (rolled up from phase reports)

None on the core mechanism (both phases reported "none"). `FREEZE_COMPILER_MAP.md` §4/§5/§9 (+ a
§7 cross-reference) and `CINEMATIC_POST_DESIGN.md`'s status line were left for this landing to
update, per the doc's own P0 brief — done in this landing, not carried as debt.

## Verification debt

None opened, none carried. This is a compiler-internals phase with no user-visible surface (L1
target, met); the doc's no-PNG rule means there is no L2 gap to record.

## Bugs logged

BUG-126 (`docs/BUG_BACKLOG.md`) — 12 pre-existing clippy findings in `manifold-renderer`'s test
code, visible only under `--tests --features gpu-proofs`, confirmed untouched by this diff.
Unrelated to P0; logged so it doesn't get attributed to this landing later.

## Click-script for Peter (≤2 minutes)

This phase has no user-visible surface — there is nothing to click. The proof is numeric: run
`cargo test -p manifold-renderer --lib node_graph::freeze::proof::camera_derived_pointwise_atom_fuses_and_matches_unfused -- --exact --nocapture`
and confirm it reports `ok` (fused and unfused render, byte-compared, identical). P1 (next phase)
will give this mechanism its first user-visible consumer — `focus_distance` bound to an LFO,
breathing a rack-focus — worth a look once that lands.
