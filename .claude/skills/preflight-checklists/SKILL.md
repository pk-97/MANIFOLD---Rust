---
name: preflight-checklists
description: Task-specific checklists to verify before committing — prevents recurring bug classes. Invoke at the end of a change, before the commit.
---
# Pre-flight Checklists

Run these checks BEFORE committing changes. Each list targets a specific task type
and addresses the exact bugs that have recurred in past sessions.

---

## Adding a New Effect

CLAUDE.md says touch 7 locations. Verify ALL of them:

- [ ] `effect_type_id.rs` — added `pub const` with `Cow::Borrowed("Name")`
- [ ] `effect_type_registry.rs` — added `reg()` entry with correct category
- [ ] `effect_definition_registry.rs` — added `EffectDef` with params
  - `param_count` matches `param_defs.len()`
  - `osc_prefix` set (lowercase, no slash)
  - Effect `pd()` signature: `(name, min, max, default)` — NO fmt/osc args
- [ ] `effect_category_registry.rs` — added category if not POST_PROCESS
- [ ] `effects/my_effect.rs` — new file implementing `PostProcessEffect`
- [ ] `effects/mod.rs` — added `pub mod my_effect;`
- [ ] `effect_registry.rs` — added `Box::new(MyEffectFX::new(device))` with correct TypeId key

**Uniform struct check:**
- [ ] `#[repr(C)]` and `bytemuck::Pod, Zeroable`
- [ ] Total size is multiple of 16 bytes (add `_pad` fields)
- [ ] Field order matches WGSL struct exactly
- [ ] `println!("size: {}", std::mem::size_of::<MyUniforms>())` matches expected

---

## Adding a New Generator

CLAUDE.md says touch 6 locations:

- [ ] `generator_type_id.rs` — added `pub const` with `Cow::Borrowed("Name")`
- [ ] `generator_type_registry.rs` — added `reg()` entry
- [ ] `generator_definition_registry.rs` — added via `create_def()`
  - Generator `pd()` signature: `(name, min, max, default, fmt, osc)` — HAS fmt and osc args
  - `pd_toggle()` signature: `(name, min, max, default, osc)`
  - `pd_whole_labels()` signature: `(name, min, max, default, &labels, osc)`
- [ ] `generators/my_gen.rs` — new file implementing `Generator`
- [ ] `generators/mod.rs` — added `pub mod my_gen;`
- [ ] `generators/registry.rs` — added to BOTH `prewarm_all()` array AND `create()` if-else chain

**Line-based generator extra checks:**
- [ ] Uses `LinePipeline` for rendering
- [ ] Vertex buffer sized correctly
- [ ] `@workgroup_size` respects Metal limits

---

## Modifying a Shader / Uniform

- [ ] Read the ACTUAL `.wgsl` file first (don't synthesize from memory — mistake #11)
- [ ] Rust struct field order matches WGSL struct field order exactly
- [ ] Total Rust struct size is 16-byte aligned
- [ ] `vec3<f32>` in WGSL = 16 bytes (NOT 12) — add padding in Rust
- [ ] `R16Float` NOT used for storage textures (no STORAGE_BINDING on Metal)
- [ ] 3D workgroup size ≤ 256 total invocations (`4,4,4` or `8,8,4` max)
- [ ] If multiple dispatches share a binding group, each gets its own uniform buffer
- [ ] `textureSample` (implicit LOD) in fragment shaders, `textureSampleLevel` in compute

---

## Modifying the Content Thread

- [ ] No `wgpu::*` types introduced (mistake #3)
- [ ] No new `Arc<Mutex<>>` or `Arc<RwLock<>>` without explicit approval
- [ ] All project mutations go through `EditingService` → `Command` or `MutateProject`
- [ ] UI parameter writes target `base_param_values`, not `param_values` (mistake #14)
- [ ] Command buffers held until GPU completion handler fires (mistake #16)
- [ ] No allocations in per-frame hot paths (`engine_tick`, `sync_clips_to_time`, render)
- [ ] `DataVersion` incremented when project state changes

---

## Modifying Compositor / Blend Pipeline

- [ ] Clips sorted descending by `layer_index` before compositor (RECURRING — mistake #9)
- [ ] Each blend pass has its own uniform buffer slot (mistake #2)
- [ ] HDR intermediates at HALF resolution minimum, never quarter (mistake #10)
- [ ] Test with 3+ layers and different blend modes

---

## Modifying VSync / Display / Presenter

- [ ] CVDisplayLink callbacks < 1μs — signal only, no GPU work (mistake #15)
- [ ] Three independent CVDisplayLinks — NEVER unify (mistake #17)
- [ ] `presentsWithTransaction` only on main thread (mistake #6)
- [ ] Hz derived from `CVTimeStamp` in callback, not at creation time
- [ ] Fullscreen = Direct Display = must present every vsync

---

## Modifying Effect/Generator Registries

- [ ] After any registry change, grep for the TypeId across all 6-7 locations
- [ ] Verify no orphaned entries (type ID exists but no implementation, or vice versa)
- [ ] Unknown type IDs stripped on project load, not defaulted (mistake #13)

---

## Before ANY Commit

- [ ] `cargo clippy --workspace -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] All `#[serde(rename_all = "camelCase")]` on serialized structs
- [ ] No `.unwrap()` on user-facing paths (only impossible states)
