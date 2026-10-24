---
name: preflight-checklists
description: Task-specific checklists to verify before committing — prevents recurring bug classes. Invoke at the end of a change, before the commit.
---
# Pre-flight Checklists

Run these checks before committing changes:

## Six General Engineering Checks
- [ ] **Beyond trivial defaults:** Tested with non-identity transforms/scales, hierarchies, empty/large inputs, and non-default settings — not just `0`/`1` unit defaults.
- [ ] **Zero cost when off/idle:** Disabled features, bypassed nodes, and stationary overlays short-circuit early and cost `0 ms` (no extra dispatches, render targets, or allocations).
- [ ] **No unasked-for side effects:** Importing, toggling, or editing one item never silently mutates cameras, unrelated sliders, or playback state.
- [ ] **Single source of truth:** Reuses existing layout constants, coordinate mappers, and schemas instead of duplicating formulas or magic pixel offsets.
- [ ] **Smallest direct solution:** No unnecessary traits, modes, or multi-phase frameworks.
- [ ] **Replace, don't layer:** Old code, dead fields, and legacy paths are deleted in the same change — no parallel `v2` paths, compatibility shims, or silent `unwrap_or` fallbacks.

## Modifying a Shader / Uniform
- [ ] Read the actual `.wgsl` file first.
- [ ] Rust `#[repr(C)]` struct field order and 16-byte alignment (`vec3<f32>` = 16 bytes) match WGSL exactly.
- [ ] `R16Float` is not used for storage textures on Metal (use `R32Float` or `Rgba16Float`).
- [ ] 3D compute `@workgroup_size` ≤ 256 total invocations (`4,4,4` or `8,8,4` max).
- [ ] If multiple dispatches share a binding group, each gets its own uniform buffer or offset.

## Modifying the Content Thread / UI Boundary
- [ ] All GPU work goes through `manifold-gpu` (`wgpu::*` never used on the content thread).
- [ ] No new `Arc<Mutex<>>` or `Arc<RwLock<>>` without explicit approval.
- [ ] All project mutations route through `EditingService` (`Command` / `MutateProject`), writing to `base_param_values`.
- [ ] No per-frame allocations on hot paths (`engine_tick`, `sync_clips_to_time`, render).

## Before Committing
- [ ] Run scoped tests: `cargo nextest run -p <touched_crate> <filter>`
- [ ] Run scoped clippy: `cargo clippy -p <touched_crate> --tests -- -D warnings`
- [ ] If node ports/params changed: `./scripts/dev.py node-catalog`
- [ ] If docs were added/renamed: `./scripts/dev.py docs-index`
