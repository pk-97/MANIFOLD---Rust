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
//! layout creation and by `GpuEncoder` binding logic: pure reflection on
//! the naga module.

use super::{Slot, SlotKind, SlotMap};
use crate::shader_common::{
    classify_global, collect_entry_point_globals, compile_to_optimized_spirv,
    parse_and_validate_wgsl, rebind_vertex_output_as_point_size,
};

/// Parse WGSL, introspect bindings, emit optimised SPIR-V for a compute
/// entry point. Returns the slot map, SPIR-V words, and the workgroup
/// size declared in the shader.
pub(super) fn compile_wgsl_to_spirv_compute(
    wgsl_source: &str,
    entry_point: &str,
    label: &str,
) -> (SlotMap, Vec<u32>, [u32; 3]) {
    let (module, info) = parse_and_validate_wgsl(wgsl_source, label);
    let slot_map = build_slot_map(&module, entry_point);

    let workgroup_size = module
        .entry_points
        .iter()
        .find(|ep| ep.name == entry_point)
        .unwrap_or_else(|| panic!("{label}: entry point '{entry_point}' not found in module"))
        .workgroup_size;

    let spirv = compile_to_optimized_spirv(&module, &info, label);
    (slot_map, spirv, workgroup_size)
}

/// Parse WGSL, introspect bindings, emit optimised SPIR-V for a render
/// pipeline (vertex + fragment entry points). Both stages share the
/// SPIR-V module; Vulkan selects the entry point at `VkPipeline` creation
/// time via `VkPipelineShaderStageCreateInfo::pName`. `point_size_location`
/// rebinds that vertex output to the point size.
pub(super) fn compile_wgsl_to_spirv_render(
    wgsl_source: &str,
    vs_entry: &str,
    fs_entry: &str,
    point_size_location: Option<u32>,
    label: &str,
) -> (SlotMap, Vec<u32>) {
    let (module, info) = parse_and_validate_wgsl(wgsl_source, label);
    let (module, info) = match point_size_location {
        Some(location) => rebind_vertex_output_as_point_size(module, vs_entry, location, label),
        None => (module, info),
    };
    let slot_map = build_slot_map_render(&module, vs_entry, fs_entry);
    let spirv = compile_to_optimized_spirv(&module, &info, label);
    (slot_map, spirv)
}

/// Build a Vulkan slot map for a single compute entry point.
fn build_slot_map(module: &naga::Module, entry_point: &str) -> SlotMap {
    // Walk entry point + all transitively called helper functions. Multi-
    // entry-point shaders (e.g. fluid_scatter.wgsl) reuse @binding(N) for
    // different types per entry, so we must only collect globals actually
    // reachable from THIS entry.
    slot_map_for(module, &collect_entry_point_globals(module, entry_point))
}

/// Build a unified Vulkan slot map for a render pipeline. Vertex +
/// fragment descriptor layouts are merged into one `VkDescriptorSetLayout`
/// at set=0 — simplest model, matches the Metal backend's unified Metal
/// argument table.
fn build_slot_map_render(module: &naga::Module, vs_entry: &str, fs_entry: &str) -> SlotMap {
    let vs_globals = collect_entry_point_globals(module, vs_entry);
    let fs_globals = collect_entry_point_globals(module, fs_entry);
    slot_map_for(module, &vs_globals.union(&fs_globals).copied().collect())
}

/// naga's SPIR-V back-end emits WGSL `@binding(N)` as Vulkan descriptor
/// `(set=0, binding=N)` without remapping, so this collects the descriptor
/// type and writability `VkDescriptorSetLayout` creation needs (D2). No
/// sizes-buffer slot: `arrayLength` is native `OpArrayLength` (D9).
fn slot_map_for(
    module: &naga::Module,
    used: &std::collections::HashSet<naga::Handle<naga::GlobalVariable>>,
) -> SlotMap {
    let mut slot_map = SlotMap::new();
    for (handle, gv) in module.global_variables.iter() {
        if !used.contains(&handle) {
            continue;
        }
        let Some(ref binding) = gv.binding else {
            continue;
        };
        let kind = classify_global(module, gv);
        let slot_kind = if kind.is_buffer {
            if gv.space == naga::AddressSpace::Uniform {
                SlotKind::UniformBuffer
            } else {
                SlotKind::StorageBuffer
            }
        } else if kind.is_texture {
            if matches!(
                module.types[gv.ty].inner,
                naga::TypeInner::Image {
                    class: naga::ImageClass::Storage { .. },
                    ..
                }
            ) {
                SlotKind::StorageImage
            } else {
                SlotKind::SampledImage
            }
        } else if kind.is_sampler {
            SlotKind::Sampler
        } else {
            // Validated WGSL has no other bound kind.
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
    }
    slot_map
}
