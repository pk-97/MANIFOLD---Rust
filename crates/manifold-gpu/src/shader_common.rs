//! Backend-neutral shader compilation primitives.
//!
//! The pipeline from WGSL source to optimised SPIR-V is identical on every
//! backend — only the final "SPIR-V → X" step diverges:
//!
//! * Metal: SPIR-V → SPIRV-Cross → MSL → `MTLLibrary` (see `metal/shader_compiler.rs`)
//! * Vulkan: SPIR-V → `vkCreateShaderModule` directly (see `vulkan/shader_compiler.rs`)
//!
//! Everything in this module is backend-agnostic: naga's WGSL front-end,
//! validation, SPIR-V back-end, the `spirv-opt` optimiser, and naga module
//! reflection helpers used to introspect bindings and transitive call graphs.
//!
//! This module is always compiled. Backend-specific emission sits in its
//! respective `metal/` or `vulkan/` module and consumes these primitives.

use std::collections::HashSet;

/// Parse WGSL and run naga's full validator.
///
/// Panics on parse or validation error — MANIFOLD treats shader source as
/// compile-time input (bundled via `include_str!`), so either the shader is
/// malformed (developer error) or naga's stricter-than-MSL validator has
/// rejected something the Metal path used to compile silently. Both should
/// surface loudly at boot rather than get swallowed at runtime.
pub(crate) fn parse_and_validate_wgsl(
    wgsl_source: &str,
    label: &str,
) -> (naga::Module, naga::valid::ModuleInfo) {
    let module = naga::front::wgsl::parse_str(wgsl_source)
        .unwrap_or_else(|e| panic!("{label}: WGSL parse error: {e}"));
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{label}: WGSL validation error: {e}"));
    (module, info)
}

/// Generate optimised SPIR-V from a naga module:
///   1. naga Module → SPIR-V (via `naga::back::spv`)
///   2. SPIR-V → optimised SPIR-V (via `spirv-tools` optimiser)
///
/// When `use_half` is true, adds `RelaxFloatOps` + `ConvertRelaxedToHalf`
/// passes that convert f32 ALU ops to f16 in the SPIR-V IR. On Metal,
/// SPIRV-Cross then emits MSL `half` types (2× ALU throughput on Apple
/// Silicon). On Vulkan, drivers consume the f16 SPIR-V directly.
pub(crate) fn compile_to_optimized_spirv(
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

/// Run `spirv-opt` optimisation passes on SPIR-V words.
/// Falls back to unoptimised SPIR-V if optimisation fails.
///
/// When `use_half` is true, two additional passes are registered:
///  - `RelaxFloatOps`: decorates all f32 results/operands with `RelaxedPrecision`
///  - `ConvertRelaxedToHalf`: converts `RelaxedPrecision` f32 ops to f16
///
/// These run after standard optimisation so constant folding and DCE have
/// already simplified the IR.
fn optimize_spirv(spv_words: &[u32], label: &str, use_half: bool) -> Vec<u32> {
    use spirv_tools::opt::{self, Optimizer};

    let mut optimizer = opt::create(None);

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
        Ok(binary) => binary.as_words().to_vec(),
        Err(e) => {
            log::warn!("{label}: spirv-opt optimization failed ({e}), using unoptimized SPIR-V");
            spv_words.to_vec()
        }
    }
}

// ─── Reflection helpers ──────────────────────────────────────────────
//
// Every backend needs to answer "which global bindings does this entry
// point (plus its transitive callees) actually touch?" — it's how we avoid
// binding-slot collisions on multi-entry-point shaders (e.g. fluid_scatter
// reuses `@binding(0)` for different types per entry). These helpers walk
// the naga module; the per-backend code then maps the discovered globals
// into its own binding table (Metal argument indices or Vulkan descriptor
// set layouts).

/// Collect `GlobalVariable` handles referenced in a function's expressions.
pub(crate) fn collect_globals_from_function(
    func: &naga::Function,
    out: &mut HashSet<naga::Handle<naga::GlobalVariable>>,
) {
    for (_, expr) in func.expressions.iter() {
        if let naga::Expression::GlobalVariable(handle) = *expr {
            out.insert(handle);
        }
    }
}

/// Recursively collect all functions called from `func` (transitive closure).
pub(crate) fn collect_called_functions(
    func: &naga::Function,
    module: &naga::Module,
    out: &mut HashSet<naga::Handle<naga::Function>>,
) {
    for (_, expr) in func.expressions.iter() {
        if let naga::Expression::CallResult(fn_handle) = *expr
            && out.insert(fn_handle)
        {
            collect_called_functions(&module.functions[fn_handle], module, out);
        }
    }
    // Also scan block statements for Call statements (not all calls have results).
    collect_calls_from_block(&func.body, module, out);
}

fn collect_calls_from_block(
    block: &naga::Block,
    module: &naga::Module,
    out: &mut HashSet<naga::Handle<naga::Function>>,
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

/// Collect the set of globals reachable from `entry_point`, walking through
/// all transitively-called helper functions. Falls back to "all globals in
/// the module" when the entry point can't be located (defensive — naga's
/// entry_points list should always match, but the fallback keeps the
/// downstream binding table populated rather than mysteriously empty).
pub(crate) fn collect_entry_point_globals(
    module: &naga::Module,
    entry_point: &str,
) -> HashSet<naga::Handle<naga::GlobalVariable>> {
    let Some(ep) = module.entry_points.iter().find(|ep| ep.name == entry_point) else {
        return module.global_variables.iter().map(|(h, _)| h).collect();
    };

    let mut called_fns: HashSet<naga::Handle<naga::Function>> = HashSet::new();
    collect_called_functions(&ep.function, module, &mut called_fns);

    let mut globals: HashSet<naga::Handle<naga::GlobalVariable>> = HashSet::new();
    collect_globals_from_function(&ep.function, &mut globals);
    for &fn_handle in &called_fns {
        collect_globals_from_function(&module.functions[fn_handle], &mut globals);
    }
    globals
}

// ─── Binding classification ──────────────────────────────────────────

/// What kind of binding a naga global represents, from the perspective of
/// a GPU backend's descriptor model. The same global is classified the same
/// way on every backend — Metal maps these to argument slot kinds, Vulkan
/// maps them to `VkDescriptorType` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GlobalKind {
    pub is_buffer: bool,
    pub is_texture: bool,
    pub is_sampler: bool,
    pub is_writable: bool,
}

/// Classify a naga `GlobalVariable` by its resource kind and writability.
/// Writability comes from the storage address-space's access flags (storage
/// buffers) or the image class (storage textures); uniform buffers and
/// sampled textures are always read-only.
pub(crate) fn classify_global(module: &naga::Module, gv: &naga::GlobalVariable) -> GlobalKind {
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

    GlobalKind {
        is_buffer,
        is_texture,
        is_sampler,
        is_writable,
    }
}
