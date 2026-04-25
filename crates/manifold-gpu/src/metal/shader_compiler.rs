//! Metal-specific shader emission: SPIR-V → SPIRV-Cross → MSL.
//!
//! The WGSL → naga → SPIR-V → `spirv-opt` prelude is shared with the Vulkan
//! backend via `crate::shader_common`. Only the final SPIR-V-to-target step
//! (SPIRV-Cross with explicit `BindTarget` mappings from our `SlotMap`)
//! lives here. Metal then compiles the MSL at runtime into a `MTLLibrary`.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{MTLFunction, MTLLibrary};

use super::*;
use crate::shader_common::{
    classify_global, collect_entry_point_globals, compile_to_optimized_spirv,
    parse_and_validate_wgsl,
};
use spirv_cross2::Compiler;
use spirv_cross2::compile::msl;

/// Parse WGSL, introspect bindings, compile to MSL for a compute entry point.
/// Returns (slot_map, msl_source, msl_entry_name, workgroup_size).
///
/// When `use_half` is true, spirv-opt applies `RelaxFloatOps` +
/// `ConvertRelaxedToHalf` passes, converting all f32 ALU to f16 (`half`)
/// in the generated MSL. Apple Silicon executes 2× f16 ops per cycle.
/// Only enable for shaders without temporal accumulation or UV-precision
/// sensitivity (e.g. bloom, blend modes — NOT fluid sim, feedback).
pub(super) fn compile_wgsl_to_msl(
    wgsl_source: &str,
    entry_point: &str,
    label: &str,
    use_half: bool,
) -> (SlotMap, String, String, [u32; 3]) {
    // Steps 1 + 2: shared — parse WGSL and run naga validation.
    let (module, info) = parse_and_validate_wgsl(wgsl_source, label);

    // Step 3: Introspect bindings and build slot map
    let (slot_map, _entry_resources) = build_slot_map(&module, entry_point);

    // Step 4: Get workgroup size from naga module
    let entry_idx = module
        .entry_points
        .iter()
        .position(|ep| ep.name == entry_point)
        .unwrap_or_else(|| panic!("{label}: entry point '{entry_point}' not found in module"));
    let workgroup_size = module.entry_points[entry_idx].workgroup_size;

    // Step 5: WGSL → SPIR-V → spirv-opt → SPIRV-Cross → MSL
    let optimized_spirv = compile_to_optimized_spirv(&module, &info, label, use_half);
    let msl_source = compile_spirv_entry_to_msl(
        &optimized_spirv,
        &module,
        &slot_map,
        entry_point,
        spirv_cross2::spirv::ExecutionModel::GLCompute,
        label,
    );

    // SPIRV-Cross preserves entry point names from SPIR-V (which come from
    // naga's SPIR-V backend preserving the original WGSL names).
    let msl_entry_name = entry_point.to_string();

    (slot_map, msl_source, msl_entry_name, workgroup_size)
}

/// Parse WGSL, introspect bindings, compile to MSL for render (vertex + fragment).
///
/// SPIRV-Cross compiles one entry point at a time, so we compile vertex and
/// fragment separately into individual MSL strings. The caller creates separate
/// Metal libraries for each.
///
/// Returns (unified_slot_map, vs_msl, fs_msl).
pub(super) fn compile_wgsl_to_msl_render(
    wgsl_source: &str,
    vs_entry: &str,
    fs_entry: &str,
    label: &str,
) -> (SlotMap, String, String) {
    let (module, info) = parse_and_validate_wgsl(wgsl_source, label);

    // Build a UNIFIED slot map from the union of both entry points' globals.
    // VS and FS share the same Metal argument table, so bindings visible in
    // either stage need slots (e.g. line pipeline: positions/edges in VS only).
    let (unified_slot_map, _resources_vs, _resources_fs) =
        build_slot_map_render(&module, vs_entry, fs_entry);

    // WGSL → SPIR-V → spirv-opt (shared) — render pipelines always use f32
    let optimized_spirv = compile_to_optimized_spirv(&module, &info, label, false);

    // Compile vertex and fragment entry points to MSL separately.
    // SPIRV-Cross's MSL backend emits one entry point per compile() call.
    let vs_msl = compile_spirv_entry_to_msl(
        &optimized_spirv,
        &module,
        &unified_slot_map,
        vs_entry,
        spirv_cross2::spirv::ExecutionModel::Vertex,
        label,
    );
    let fs_msl = compile_spirv_entry_to_msl(
        &optimized_spirv,
        &module,
        &unified_slot_map,
        fs_entry,
        spirv_cross2::spirv::ExecutionModel::Fragment,
        label,
    );

    (unified_slot_map, vs_msl, fs_msl)
}

/// Build a SlotMap and naga EntryPointResources from a naga module.
///
/// Iterates over global variables used by the entry point and assigns
/// sequential Metal argument indices per resource type:
/// - Buffers (uniform + storage) → buffer(0), buffer(1), ...
/// - Textures (sampled + storage) → texture(0), texture(1), ...
/// - Samplers → sampler(0), sampler(1), ...
fn build_slot_map(
    module: &naga::Module,
    entry_point: &str,
) -> (SlotMap, naga::back::msl::EntryPointResources) {
    use naga::back::msl;

    let mut slot_map = SlotMap::new();
    let mut resources = msl::EntryPointResources::default();

    let mut next_buffer: u32 = 0;
    let mut next_texture: u32 = 0;
    let mut next_sampler: u32 = 0;

    // Scan entry point + all transitively called helper functions for global
    // variable references. Multi-entry-point shaders (e.g. fluid_scatter.wgsl)
    // reuse @binding(N) for different types per entry point — we must only
    // map the ones reachable from THIS entry.
    let used_globals = collect_entry_point_globals(module, entry_point);

    // Collect bindings only for globals referenced by this entry point
    let mut bindings: Vec<(u32, naga::ResourceBinding, &naga::GlobalVariable)> = Vec::new();
    for (handle, gv) in module.global_variables.iter() {
        if let Some(ref binding) = gv.binding
            && used_globals.contains(&handle)
        {
            bindings.push((binding.binding, *binding, gv));
        }
    }
    // Sort by binding number for deterministic index assignment
    bindings.sort_by_key(|(b, _, _)| *b);

    for (binding_num, resource_binding, gv) in &bindings {
        let kind = classify_global(module, gv);
        let mut bind_target = msl::BindTarget::default();

        if kind.is_buffer {
            let idx = next_buffer;
            next_buffer += 1;
            bind_target.buffer = Some(idx as u8);
            bind_target.mutable = kind.is_writable;
            slot_map.insert(
                *binding_num,
                Slot {
                    kind: SlotKind::Buffer,
                    metal_index: idx,
                },
            );
        } else if kind.is_sampler {
            let idx = next_sampler;
            next_sampler += 1;
            bind_target.sampler = Some(msl::BindSamplerTarget::Resource(idx as u8));
            slot_map.insert(
                *binding_num,
                Slot {
                    kind: SlotKind::Sampler,
                    metal_index: idx,
                },
            );
        } else if kind.is_texture {
            let idx = next_texture;
            next_texture += 1;
            bind_target.texture = Some(idx as u8);
            bind_target.mutable = kind.is_writable;
            slot_map.insert(
                *binding_num,
                Slot {
                    kind: SlotKind::Texture,
                    metal_index: idx,
                },
            );
        }

        resources.resources.insert(*resource_binding, bind_target);
    }

    if module_uses_runtime_array(module, bindings.iter().map(|(_, _, gv)| *gv)) {
        // Assign the sizes buffer to the next available buffer index.
        resources.sizes_buffer = Some(next_buffer as u8);
        // Store in slot map so dispatch can bind it.
        slot_map.insert(
            SIZES_BUFFER_BINDING,
            Slot {
                kind: SlotKind::Buffer,
                metal_index: next_buffer,
            },
        );
        next_buffer += 1;
    }

    let _ = next_buffer; // final slot counter only needed while assigning above

    (slot_map, resources)
}

/// Detect runtime-sized arrays in storage buffers (top-level `array<T>`,
/// struct with last member `array<T>`, or binding-array of resources).
/// naga's MSL backend needs a "sizes buffer" containing the byte size of
/// each runtime-sized buffer so it can resolve `arrayLength()` calls. If
/// any global needs it, the caller should reserve a slot at
/// `SIZES_BUFFER_BINDING` and the encoder side will populate it on dispatch.
///
/// Shared between compute (`build_slot_map`) and render (`build_slot_map_render`)
/// — without this in the render path, fragment shaders that use
/// `arrayLength()` (e.g. analyzer's `weighting_db()` reading the LUT)
/// silently see length 0 and any `n < 2` early-out collapses.
fn module_uses_runtime_array<'a>(
    module: &'a naga::Module,
    globals: impl IntoIterator<Item = &'a naga::GlobalVariable>,
) -> bool {
    globals.into_iter().any(|gv| {
        matches!(gv.space, naga::AddressSpace::Storage { .. }) && {
            let ty = &module.types[gv.ty];
            match &ty.inner {
                naga::TypeInner::Array {
                    size: naga::ArraySize::Dynamic,
                    ..
                } => true,
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
        }
    })
}

/// Build a unified SlotMap + per-entry-point EntryPointResources for a render
/// pipeline (vertex + fragment). Both stages share the same Metal argument table,
/// so the slot map includes globals from the union of both entry points.
/// Each stage gets its own EntryPointResources with the shared index assignments.
fn build_slot_map_render(
    module: &naga::Module,
    vs_entry: &str,
    fs_entry: &str,
) -> (
    SlotMap,
    naga::back::msl::EntryPointResources,
    naga::back::msl::EntryPointResources,
) {
    use naga::back::msl;

    // Union of the globals reachable from vertex and fragment entry points.
    // Both stages share the same Metal argument table, so bindings visible in
    // either stage need slots in the unified `SlotMap`.
    let vs_globals = collect_entry_point_globals(module, vs_entry);
    let fs_globals = collect_entry_point_globals(module, fs_entry);
    let all_globals: std::collections::HashSet<_> =
        vs_globals.union(&fs_globals).copied().collect();

    // Collect bindings from the union
    let mut bindings: Vec<(u32, naga::ResourceBinding, &naga::GlobalVariable)> = Vec::new();
    for (handle, gv) in module.global_variables.iter() {
        if let Some(ref binding) = gv.binding
            && all_globals.contains(&handle)
        {
            bindings.push((binding.binding, *binding, gv));
        }
    }
    bindings.sort_by_key(|(b, _, _)| *b);

    // Build unified slot map + per-entry-point resources with shared indices
    let mut slot_map = SlotMap::new();
    let mut resources_vs = msl::EntryPointResources::default();
    let mut resources_fs = msl::EntryPointResources::default();
    let mut next_buffer: u32 = 0;
    let mut next_texture: u32 = 0;
    let mut next_sampler: u32 = 0;

    for (binding_num, resource_binding, gv) in &bindings {
        let kind = classify_global(module, gv);
        let mut bind_target = msl::BindTarget::default();

        if kind.is_buffer {
            let idx = next_buffer;
            next_buffer += 1;
            bind_target.buffer = Some(idx as u8);
            bind_target.mutable = kind.is_writable;
            slot_map.insert(
                *binding_num,
                Slot {
                    kind: SlotKind::Buffer,
                    metal_index: idx,
                },
            );
        } else if kind.is_sampler {
            let idx = next_sampler;
            next_sampler += 1;
            bind_target.sampler = Some(msl::BindSamplerTarget::Resource(idx as u8));
            slot_map.insert(
                *binding_num,
                Slot {
                    kind: SlotKind::Sampler,
                    metal_index: idx,
                },
            );
        } else if kind.is_texture {
            let idx = next_texture;
            next_texture += 1;
            bind_target.texture = Some(idx as u8);
            bind_target.mutable = kind.is_writable;
            slot_map.insert(
                *binding_num,
                Slot {
                    kind: SlotKind::Texture,
                    metal_index: idx,
                },
            );
        }

        // Add to both entry points' resources — naga + fake_missing_bindings
        // handles the case where a binding is only used in one stage.
        resources_vs
            .resources
            .insert(*resource_binding, bind_target.clone());
        resources_fs
            .resources
            .insert(*resource_binding, bind_target);
    }

    // Same runtime-array → sizes-buffer reservation as the compute path.
    // Without this any fragment shader that calls `arrayLength()` on a
    // runtime-sized storage array (e.g. analyzer's `weighting_db()`) reads
    // length 0, silently collapsing the look-up to its zero-init guard.
    // Both stages share the Metal argument table so we reserve once and
    // expose it to both EntryPointResources.
    if module_uses_runtime_array(module, bindings.iter().map(|(_, _, gv)| *gv)) {
        let sizes_idx = next_buffer;
        resources_vs.sizes_buffer = Some(sizes_idx as u8);
        resources_fs.sizes_buffer = Some(sizes_idx as u8);
        slot_map.insert(
            SIZES_BUFFER_BINDING,
            Slot {
                kind: SlotKind::Buffer,
                metal_index: sizes_idx,
            },
        );
        next_buffer += 1;
    }
    let _ = next_buffer;

    (slot_map, resources_vs, resources_fs)
}

/// Compile optimized SPIR-V to MSL via SPIRV-Cross for a single entry point.
///
/// SPIRV-Cross's MSL backend compiles one entry point at a time.
/// For multi-entry-point modules (render pipelines), call this once per entry point
/// and create separate Metal libraries for each.
fn compile_spirv_entry_to_msl(
    spv_words: &[u32],
    naga_module: &naga::Module,
    slot_map: &SlotMap,
    entry_point: &str,
    exec_model: spirv_cross2::spirv::ExecutionModel,
    label: &str,
) -> String {
    use spirv_cross2::Module;
    use spirv_cross2::targets::Msl;

    let sc_module = Module::from_words(spv_words);
    let mut compiler: Compiler<Msl> = Compiler::new(sc_module)
        .unwrap_or_else(|e| panic!("{label}: SPIRV-Cross compiler creation error: {e}"));

    // Set the active entry point
    compiler
        .set_entry_point(entry_point, exec_model)
        .unwrap_or_else(|e| {
            panic!("{label}: SPIRV-Cross set_entry_point('{entry_point}') error: {e}")
        });

    // Add explicit resource bindings matching our SlotMap.
    add_resource_bindings_from_slot_map(&mut compiler, naga_module, slot_map, exec_model, label);

    // Configure MSL compiler options
    let mut options = <Msl as spirv_cross2::compile::CompilableTarget>::options();
    options.version = msl::MslVersion::new(2, 4, 0);
    options.platform = msl::MetalPlatform::MacOS;
    options.force_native_arrays = true;

    let artifact = compiler
        .compile(&options)
        .unwrap_or_else(|e| panic!("{label}: SPIRV-Cross MSL compilation error: {e}"));

    artifact.to_string()
}

/// Add explicit MSL resource bindings to SPIRV-Cross compiler matching our SlotMap.
///
/// Iterates over naga module globals, finds their WGSL @binding(N), looks up the
/// Metal argument index from the SlotMap, and tells SPIRV-Cross to use that index.
fn add_resource_bindings_from_slot_map(
    compiler: &mut Compiler<spirv_cross2::targets::Msl>,
    naga_module: &naga::Module,
    slot_map: &SlotMap,
    exec_model: spirv_cross2::spirv::ExecutionModel,
    label: &str,
) {
    for (_handle, gv) in naga_module.global_variables.iter() {
        let Some(ref binding) = gv.binding else {
            continue;
        };
        let wgsl_binding = binding.binding;
        let Some(slot) = slot_map.get(wgsl_binding) else {
            continue;
        };

        // SPIRV-Cross uses descriptor set 0 for all bindings (naga puts
        // everything in set 0 when outputting SPIR-V).
        let resource_binding = msl::ResourceBinding::Qualified {
            set: binding.group,
            binding: wgsl_binding,
        };
        let mut bind_target = msl::BindTarget {
            buffer: 0,
            texture: 0,
            sampler: 0,
            count: None,
        };
        match slot.kind {
            SlotKind::Buffer => bind_target.buffer = slot.metal_index,
            SlotKind::Texture => bind_target.texture = slot.metal_index,
            SlotKind::Sampler => bind_target.sampler = slot.metal_index,
        }

        if let Err(e) = compiler.add_resource_binding(exec_model, resource_binding, &bind_target) {
            log::warn!("{label}: failed to set MSL binding for @binding({wgsl_binding}): {e}");
        }
    }

    // If there's a sizes buffer in the slot map, add it too
    if let Some(sizes_slot) = slot_map.get(SIZES_BUFFER_BINDING) {
        let resource_binding = msl::ResourceBinding::BufferSizeBuffer(sizes_slot.metal_index);
        let bind_target = msl::BindTarget {
            buffer: sizes_slot.metal_index,
            texture: 0,
            sampler: 0,
            count: None,
        };
        if let Err(e) = compiler.add_resource_binding(exec_model, resource_binding, &bind_target) {
            log::warn!("{label}: failed to set MSL sizes buffer binding: {e}");
        }
    }
}

/// Find an entry function in a Metal library. Tries the exact name first,
/// then looks for naga-mangled versions (e.g. "cs_main" → "cs_main_").
pub(super) fn find_entry_function(
    library: &ProtocolObject<dyn MTLLibrary>,
    entry_name: &str,
    available: &[String],
    label: &str,
    stage: &str,
) -> Retained<ProtocolObject<dyn MTLFunction>> {
    // Try exact name
    let entry_ns = NSString::from_str(entry_name);
    if let Some(f) = library.newFunctionWithName(&entry_ns) {
        return f;
    }
    // Try with underscore suffix (naga sometimes appends)
    let mangled = format!("{entry_name}_");
    let mangled_ns = NSString::from_str(&mangled);
    if let Some(f) = library.newFunctionWithName(&mangled_ns) {
        return f;
    }
    // Try matching prefix
    for name in available {
        if name.starts_with(entry_name) {
            let candidate = NSString::from_str(name);
            if let Some(f) = library.newFunctionWithName(&candidate) {
                return f;
            }
        }
    }
    panic!("{label}: {stage} function '{entry_name}' not found. Available: {available:?}");
}
