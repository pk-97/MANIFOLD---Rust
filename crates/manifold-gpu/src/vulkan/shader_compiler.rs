//! Vulkan shader emission: WGSL → SPIR-V → (`vkCreateShaderModule`).
//!
//! The WGSL → naga → SPIR-V → `spirv-opt` prelude comes from
//! `crate::shader_common` — shared with the Metal backend. From there the
//! Vulkan path is short: naga's SPIR-V back-end already emits
//! `(set=0, binding=N)` decorations matching the original WGSL bindings,
//! so the output is ready to hand to `vkCreateShaderModule` without a
//! SPIRV-Cross translation step.
//!
//! This file builds the per-pipeline `SlotMap` used by descriptor set
//! layout creation (Phase 1) and by `GpuEncoder` binding logic. The
//! SlotMap construction is pure reflection on the naga module — no
//! Vulkan calls needed — so it lives here in Phase 0 rather than waiting
//! for Phase 1.

use super::{SIZES_BUFFER_BINDING, Slot, SlotKind, SlotMap};
use crate::shader_common::{
    classify_global, collect_entry_point_globals, compile_to_optimized_spirv,
    parse_and_validate_wgsl,
};

/// Parse WGSL, introspect bindings, emit optimised SPIR-V for a compute
/// entry point. Returns the slot map, SPIR-V words, and the workgroup
/// size declared in the shader.
///
/// Unused in Phase 0 (no consumer yet — `vulkan::device::GpuDevice` calls
/// this in Phase 1 when building `VkComputePipeline`s). Kept here so the
/// shader-side logic lives in one place.
#[allow(dead_code)]
pub(super) fn compile_wgsl_to_spirv_compute(
    wgsl_source: &str,
    entry_point: &str,
    label: &str,
    use_half: bool,
) -> (SlotMap, Vec<u32>, [u32; 3]) {
    let (module, info) = parse_and_validate_wgsl(wgsl_source, label);
    let slot_map = build_slot_map(&module, entry_point);

    let workgroup_size = module
        .entry_points
        .iter()
        .find(|ep| ep.name == entry_point)
        .unwrap_or_else(|| panic!("{label}: entry point '{entry_point}' not found in module"))
        .workgroup_size;

    let spirv = compile_to_optimized_spirv(&module, &info, label, use_half);
    (slot_map, spirv, workgroup_size)
}

/// Parse WGSL, introspect bindings, emit optimised SPIR-V for a render
/// pipeline (vertex + fragment entry points). Both stages share the
/// SPIR-V module; Vulkan selects the entry point at `VkPipeline` creation
/// time via `VkPipelineShaderStageCreateInfo::pName`.
///
/// Unused in Phase 0. Lands on the hot path when `GpuDevice::create_render_pipeline`
/// is implemented.
#[allow(dead_code)]
pub(super) fn compile_wgsl_to_spirv_render(
    wgsl_source: &str,
    vs_entry: &str,
    fs_entry: &str,
    label: &str,
) -> (SlotMap, Vec<u32>) {
    let (module, info) = parse_and_validate_wgsl(wgsl_source, label);
    let slot_map = build_slot_map_render(&module, vs_entry, fs_entry);
    // Render pipelines don't get the f32→f16 relax pass today — matches
    // the Metal backend's `compile_wgsl_to_msl_render(..., use_half=false)`.
    let spirv = compile_to_optimized_spirv(&module, &info, label, false);
    (slot_map, spirv)
}

/// Build a Vulkan slot map for a single compute entry point.
///
/// naga's SPIR-V back-end emits WGSL `@binding(N)` as Vulkan descriptor
/// `(set=0, binding=N)` without remapping, so this pass is primarily
/// collecting per-binding metadata (descriptor kind + writability) that
/// `VkDescriptorSetLayout` creation needs — plus the synthetic sizes
/// buffer slot for shaders that call `arrayLength()`.
fn build_slot_map(module: &naga::Module, entry_point: &str) -> SlotMap {
    let mut slot_map = SlotMap::new();

    // Walk entry point + all transitively called helper functions. Multi-
    // entry-point shaders (e.g. fluid_scatter.wgsl) reuse @binding(N) for
    // different types per entry, so we must only collect globals actually
    // reachable from THIS entry.
    let used_globals = collect_entry_point_globals(module, entry_point);

    // Next descriptor binding to assign when we find the synthetic sizes
    // buffer. Starts one past the highest user-declared binding so it
    // can't collide with a real WGSL binding number.
    let mut next_binding: u32 = 0;

    for (handle, gv) in module.global_variables.iter() {
        if !used_globals.contains(&handle) {
            continue;
        }
        let Some(ref binding) = gv.binding else {
            continue;
        };
        let kind = classify_global(module, gv);

        let slot_kind = if kind.is_buffer {
            SlotKind::Buffer
        } else if kind.is_texture {
            SlotKind::Texture
        } else if kind.is_sampler {
            SlotKind::Sampler
        } else {
            // Global without a backend-recognised kind — skip rather than
            // guessing. Shouldn't happen for validated WGSL; defensive.
            continue;
        };

        slot_map.insert(
            binding.binding,
            Slot {
                kind: slot_kind,
                vulkan_binding: binding.binding,
                writable: kind.is_writable,
            },
        );
        next_binding = next_binding.max(binding.binding + 1);
    }

    if has_runtime_sized_storage(module, &used_globals) {
        slot_map.insert(
            SIZES_BUFFER_BINDING,
            Slot {
                kind: SlotKind::Buffer,
                vulkan_binding: next_binding,
                writable: false,
            },
        );
    }

    slot_map
}

/// Build a unified Vulkan slot map for a render pipeline. Vertex +
/// fragment descriptor layouts are merged into one `VkDescriptorSetLayout`
/// at set=0 — simplest model, matches the Metal backend's unified Metal
/// argument table.
fn build_slot_map_render(module: &naga::Module, vs_entry: &str, fs_entry: &str) -> SlotMap {
    let mut slot_map = SlotMap::new();

    let vs_globals = collect_entry_point_globals(module, vs_entry);
    let fs_globals = collect_entry_point_globals(module, fs_entry);
    let all_globals: std::collections::HashSet<_> =
        vs_globals.union(&fs_globals).copied().collect();

    let mut next_binding: u32 = 0;

    for (handle, gv) in module.global_variables.iter() {
        if !all_globals.contains(&handle) {
            continue;
        }
        let Some(ref binding) = gv.binding else {
            continue;
        };
        let kind = classify_global(module, gv);

        let slot_kind = if kind.is_buffer {
            SlotKind::Buffer
        } else if kind.is_texture {
            SlotKind::Texture
        } else if kind.is_sampler {
            SlotKind::Sampler
        } else {
            continue;
        };

        slot_map.insert(
            binding.binding,
            Slot {
                kind: slot_kind,
                vulkan_binding: binding.binding,
                writable: kind.is_writable,
            },
        );
        next_binding = next_binding.max(binding.binding + 1);
    }

    if has_runtime_sized_storage(module, &all_globals) {
        slot_map.insert(
            SIZES_BUFFER_BINDING,
            Slot {
                kind: SlotKind::Buffer,
                vulkan_binding: next_binding,
                writable: false,
            },
        );
    }

    slot_map
}

/// True if any of the used globals is a runtime-sized storage array — the
/// only case where naga's SPIR-V back-end emits an `arrayLength()` call
/// that needs the synthetic sizes buffer to resolve.
fn has_runtime_sized_storage(
    module: &naga::Module,
    used: &std::collections::HashSet<naga::Handle<naga::GlobalVariable>>,
) -> bool {
    module.global_variables.iter().any(|(h, gv)| {
        if !used.contains(&h) {
            return false;
        }
        if !matches!(gv.space, naga::AddressSpace::Storage { .. }) {
            return false;
        }
        let ty = &module.types[gv.ty];
        match &ty.inner {
            // Top-level runtime array: `var<storage> foo: array<T>`.
            naga::TypeInner::Array {
                size: naga::ArraySize::Dynamic,
                ..
            } => true,
            // Struct with last member a runtime array.
            naga::TypeInner::Struct { members, .. } => members.last().is_some_and(|m| {
                matches!(
                    module.types[m.ty].inner,
                    naga::TypeInner::Array {
                        size: naga::ArraySize::Dynamic,
                        ..
                    }
                )
            }),
            naga::TypeInner::BindingArray {
                size: naga::ArraySize::Dynamic,
                ..
            } => true,
            _ => false,
        }
    })
}
