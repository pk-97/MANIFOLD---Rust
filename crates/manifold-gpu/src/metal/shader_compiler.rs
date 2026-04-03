//! WGSL → SPIR-V → spirv-opt → SPIRV-Cross → MSL compilation pipeline.
//!
//! naga parses WGSL and provides binding introspection for the SlotMap.
//! spirv-opt runs optimization passes (constant folding, dead code elimination, etc.).
//! SPIRV-Cross compiles optimized SPIR-V to MSL with explicit resource binding indices
//! matching the SlotMap assignments. Metal compiles MSL at runtime.

use super::*;
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
    // Step 1: Parse WGSL
    let module = naga::front::wgsl::parse_str(wgsl_source)
        .unwrap_or_else(|e| panic!("{label}: WGSL parse error: {e}"));

    // Step 2: Validate
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{label}: WGSL validation error: {e}"));

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
    let module = naga::front::wgsl::parse_str(wgsl_source)
        .unwrap_or_else(|e| panic!("{label}: WGSL parse error: {e}"));
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{label}: WGSL validation error: {e}"));

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

    // Find which global variables are actually used by this entry point.
    // Multi-entry-point shaders (e.g. fluid_scatter.wgsl) reuse @binding(N)
    // for different types per entry point — we must only map the ones used.
    let ep = module.entry_points.iter().find(|ep| ep.name == entry_point);

    // Scan entry point AND all reachable functions for GlobalVariable references.
    // The entry point's function body may call helper functions that reference
    // globals (e.g. bloom_compute.wgsl: cs_main → blur13 → source_tex_b).
    // We must include globals from called functions too, or bindings get dropped.
    let used_globals: std::collections::HashSet<naga::Handle<naga::GlobalVariable>> =
        if let Some(ep) = ep {
            // First collect all functions called from the entry point (transitively).
            let mut called_fns: std::collections::HashSet<naga::Handle<naga::Function>> =
                std::collections::HashSet::new();
            collect_called_functions(&ep.function, module, &mut called_fns);

            // Scan entry point + all called functions for GlobalVariable refs.
            let mut globals: std::collections::HashSet<naga::Handle<naga::GlobalVariable>> =
                std::collections::HashSet::new();
            collect_globals_from_function(&ep.function, &mut globals);
            for &fn_handle in &called_fns {
                collect_globals_from_function(&module.functions[fn_handle], &mut globals);
            }
            globals
        } else {
            // Fallback: include all globals if entry point not found
            module.global_variables.iter().map(|(h, _)| h).collect()
        };

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
        let ty = &module.types[gv.ty];
        let is_buffer = matches!(
            gv.space,
            naga::AddressSpace::Uniform | naga::AddressSpace::Storage { .. }
        );
        let is_sampler = matches!(ty.inner, naga::TypeInner::Sampler { .. });
        let is_texture = matches!(ty.inner, naga::TypeInner::Image { .. });

        let is_writable = match gv.space {
            naga::AddressSpace::Storage { access } => access.contains(naga::StorageAccess::STORE),
            _ => false,
        } || matches!(
            ty.inner,
            naga::TypeInner::Image {
                class: naga::ImageClass::Storage { access, .. },
                ..
            } if access.contains(naga::StorageAccess::STORE)
        );

        let mut bind_target = msl::BindTarget::default();

        if is_buffer {
            let idx = next_buffer;
            next_buffer += 1;
            bind_target.buffer = Some(idx as u8);
            bind_target.mutable = is_writable;
            slot_map.insert(
                *binding_num,
                Slot {
                    kind: SlotKind::Buffer,
                    metal_index: idx,
                },
            );
        } else if is_sampler {
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
        } else if is_texture {
            let idx = next_texture;
            next_texture += 1;
            bind_target.texture = Some(idx as u8);
            bind_target.mutable = is_writable;
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

    // Detect runtime-sized arrays in storage buffers.
    // naga's MSL backend needs a "sizes buffer" containing the byte size of each
    // runtime-sized buffer so it can resolve arrayLength() calls.
    // Covers both top-level `array<T>` and struct with last member `array<T>`.
    let has_runtime_array = bindings.iter().any(|(_, _, gv)| {
        matches!(gv.space, naga::AddressSpace::Storage { .. }) && {
            let ty = &module.types[gv.ty];
            match &ty.inner {
                // Top-level runtime-sized array: var<storage> foo: array<T>
                naga::TypeInner::Array {
                    size: naga::ArraySize::Dynamic,
                    ..
                } => true,
                // Struct with last member being a runtime-sized array
                naga::TypeInner::Struct { members, .. } => members.last().is_some_and(|m| {
                    matches!(
                        module.types[m.ty].inner,
                        naga::TypeInner::Array {
                            size: naga::ArraySize::Dynamic,
                            ..
                        }
                    )
                }),
                // Binding array (runtime array of resources)
                naga::TypeInner::BindingArray {
                    size: naga::ArraySize::Dynamic,
                    ..
                } => true,
                _ => false,
            }
        }
    });

    if has_runtime_array {
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

    let _ = (ep, next_buffer); // suppress unused warnings

    (slot_map, resources)
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

    // Collect globals from both entry points
    fn collect_ep_globals(
        module: &naga::Module,
        entry_name: &str,
    ) -> std::collections::HashSet<naga::Handle<naga::GlobalVariable>> {
        let ep = module.entry_points.iter().find(|ep| ep.name == entry_name);
        if let Some(ep) = ep {
            let mut called_fns: std::collections::HashSet<naga::Handle<naga::Function>> =
                std::collections::HashSet::new();
            collect_called_functions(&ep.function, module, &mut called_fns);
            let mut globals: std::collections::HashSet<naga::Handle<naga::GlobalVariable>> =
                std::collections::HashSet::new();
            collect_globals_from_function(&ep.function, &mut globals);
            for &fn_handle in &called_fns {
                collect_globals_from_function(&module.functions[fn_handle], &mut globals);
            }
            globals
        } else {
            module.global_variables.iter().map(|(h, _)| h).collect()
        }
    }

    let vs_globals = collect_ep_globals(module, vs_entry);
    let fs_globals = collect_ep_globals(module, fs_entry);

    // Union of both entry points' globals
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
        let ty = &module.types[gv.ty];
        let is_buffer = matches!(
            gv.space,
            naga::AddressSpace::Uniform | naga::AddressSpace::Storage { .. }
        );
        let is_sampler = matches!(ty.inner, naga::TypeInner::Sampler { .. });
        let is_texture = matches!(ty.inner, naga::TypeInner::Image { .. });
        let is_writable = match gv.space {
            naga::AddressSpace::Storage { access } => access.contains(naga::StorageAccess::STORE),
            _ => false,
        } || matches!(
            ty.inner,
            naga::TypeInner::Image {
                class: naga::ImageClass::Storage { access, .. },
                ..
            } if access.contains(naga::StorageAccess::STORE)
        );

        let mut bind_target = msl::BindTarget::default();

        if is_buffer {
            let idx = next_buffer;
            next_buffer += 1;
            bind_target.buffer = Some(idx as u8);
            bind_target.mutable = is_writable;
            slot_map.insert(
                *binding_num,
                Slot {
                    kind: SlotKind::Buffer,
                    metal_index: idx,
                },
            );
        } else if is_sampler {
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
        } else if is_texture {
            let idx = next_texture;
            next_texture += 1;
            bind_target.texture = Some(idx as u8);
            bind_target.mutable = is_writable;
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

    (slot_map, resources_vs, resources_fs)
}

/// Generate optimized SPIR-V from a naga module:
///   1. naga Module → SPIR-V (via naga::back::spv)
///   2. SPIR-V → optimized SPIR-V (via spirv-tools optimizer)
///
/// When `use_half` is true, adds RelaxFloatOps + ConvertRelaxedToHalf passes
/// that convert f32 ALU ops to f16 in the SPIR-V IR. SPIRV-Cross then emits
/// MSL `half` types, giving 2× ALU throughput on Apple Silicon GPUs.
fn compile_to_optimized_spirv(
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
    label: &str,
    use_half: bool,
) -> Vec<u32> {
    let spv_options = naga::back::spv::Options {
        lang_version: (1, 3),
        flags: naga::back::spv::WriterFlags::empty(),
        ..Default::default()
    };
    let spv_words = naga::back::spv::write_vec(module, info, &spv_options, None)
        .unwrap_or_else(|e| panic!("{label}: naga SPIR-V output error: {e}"));

    optimize_spirv(&spv_words, label, use_half)
}

/// Run spirv-opt optimization passes on SPIR-V words.
/// Falls back to unoptimized SPIR-V if optimization fails.
///
/// When `use_half` is true, two additional passes are registered:
///  - `RelaxFloatOps`: decorates all f32 results/operands with RelaxedPrecision
///  - `ConvertRelaxedToHalf`: converts RelaxedPrecision f32 ops to f16
///
///   These run after standard optimization so constant folding and DCE have
///   already simplified the IR. SPIRV-Cross translates f16 → MSL `half`.
fn optimize_spirv(spv_words: &[u32], label: &str, use_half: bool) -> Vec<u32> {
    use spirv_tools::opt::{self, Optimizer};

    let mut optimizer = opt::create(None);

    // Register key optimization passes (same categories as spirv-opt -O):
    optimizer
        .register_pass(opt::Passes::InlineExhaustive)
        .register_pass(opt::Passes::EliminateDeadFunctions)
        .register_pass(opt::Passes::EliminateDeadConstant)
        .register_pass(opt::Passes::EliminateDeadMembers)
        .register_pass(opt::Passes::DeadVariableElimination)
        .register_pass(opt::Passes::ConditionalConstantPropagation)
        .register_pass(opt::Passes::AggressiveDCE)
        .register_pass(opt::Passes::Simplification)
        .register_pass(opt::Passes::StrengthReduction)
        .register_pass(opt::Passes::BlockMerge)
        .register_pass(opt::Passes::CFGCleanup)
        .register_pass(opt::Passes::LocalSingleStoreElim)
        .register_pass(opt::Passes::LocalMultiStoreElim)
        .register_pass(opt::Passes::LocalAccessChainConvert)
        .register_pass(opt::Passes::InsertExtractElim)
        .register_pass(opt::Passes::CopyPropagateArrays)
        .register_pass(opt::Passes::VectorDCE)
        .register_pass(opt::Passes::RedundancyElimination)
        .register_pass(opt::Passes::ReduceLoadSize)
        .register_pass(opt::Passes::CombineAccessChains)
        .register_pass(opt::Passes::CodeSinking)
        .register_pass(opt::Passes::CompactIds);

    // Half-precision passes: mark all f32 ops as relaxable, then convert to f16.
    // SPIRV-Cross emits MSL `half`/`half4` for f16 types, giving 2× ALU
    // throughput on Apple Silicon GPUs. Only safe for non-accumulative shaders.
    if use_half {
        optimizer
            .register_pass(opt::Passes::RelaxFloatOps)
            .register_pass(opt::Passes::ConvertRelaxedToHalf);
    }

    match optimizer.optimize(
        spv_words,
        &mut |msg| {
            log::warn!("{label}: spirv-opt: {msg:?}");
        },
        None,
    ) {
        Ok(binary) => {
            // binary.as_words() gives us &[u32]
            binary.as_words().to_vec()
        }
        Err(e) => {
            log::warn!("{label}: spirv-opt optimization failed ({e}), using unoptimized SPIR-V");
            spv_words.to_vec()
        }
    }
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

/// Collect GlobalVariable handles referenced in a function's expressions.
fn collect_globals_from_function(
    func: &naga::Function,
    out: &mut std::collections::HashSet<naga::Handle<naga::GlobalVariable>>,
) {
    for (_, expr) in func.expressions.iter() {
        if let naga::Expression::GlobalVariable(handle) = *expr {
            out.insert(handle);
        }
    }
}

/// Recursively collect all functions called from `func` (transitive closure).
fn collect_called_functions(
    func: &naga::Function,
    module: &naga::Module,
    out: &mut std::collections::HashSet<naga::Handle<naga::Function>>,
) {
    for (_, expr) in func.expressions.iter() {
        if let naga::Expression::CallResult(fn_handle) = *expr
            && out.insert(fn_handle)
        {
            collect_called_functions(&module.functions[fn_handle], module, out);
        }
    }
    // Also scan block statements for Call statements (not all calls have results)
    collect_calls_from_block(&func.body, module, out);
}

/// Scan a naga Block for Call statements and collect called function handles.
fn collect_calls_from_block(
    block: &naga::Block,
    module: &naga::Module,
    out: &mut std::collections::HashSet<naga::Handle<naga::Function>>,
) {
    for stmt in block.iter() {
        match *stmt {
            naga::Statement::Call { function, .. } => {
                if out.insert(function) {
                    collect_called_functions(&module.functions[function], module, out);
                }
            }
            naga::Statement::Block(ref inner) => {
                collect_calls_from_block(inner, module, out);
            }
            naga::Statement::If {
                ref accept,
                ref reject,
                ..
            } => {
                collect_calls_from_block(accept, module, out);
                collect_calls_from_block(reject, module, out);
            }
            naga::Statement::Switch { ref cases, .. } => {
                for case in cases {
                    collect_calls_from_block(&case.body, module, out);
                }
            }
            naga::Statement::Loop {
                ref body,
                ref continuing,
                ..
            } => {
                collect_calls_from_block(body, module, out);
                collect_calls_from_block(continuing, module, out);
            }
            _ => {}
        }
    }
}

/// Find an entry function in a Metal library. Tries the exact name first,
/// then looks for naga-mangled versions (e.g. "cs_main" → "cs_main_").
pub(super) fn find_entry_function(
    library: &metal::LibraryRef,
    entry_name: &str,
    available: &[String],
    label: &str,
    stage: &str,
) -> metal::Function {
    // Try exact name
    if let Ok(f) = library.get_function(entry_name, None) {
        return f;
    }
    // Try with underscore suffix (naga sometimes appends)
    let mangled = format!("{entry_name}_");
    if let Ok(f) = library.get_function(&mangled, None) {
        return f;
    }
    // Try matching prefix
    for name in available {
        if name.starts_with(entry_name)
            && let Ok(f) = library.get_function(name, None)
        {
            return f;
        }
    }
    panic!("{label}: {stage} function '{entry_name}' not found. Available: {available:?}");
}
