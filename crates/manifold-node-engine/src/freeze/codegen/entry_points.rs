use crate::ports::PortType;

use super::types::{is_texture_input, wgsl_storage_token, CodegenError};
use super::standalone::{
    generate_standalone, generate_standalone_buffer_with_options,
    generate_standalone_resolve,
    StandaloneKernelSpec,
};

/// Generate the standalone kernel for a primitive type — the single-source
/// `run()` path. Reads the body + classification + ports/params off the type's
/// `PrimitiveSpec` consts. Deterministic, so `create_compute_pipeline` caches
/// the result across instances and sessions (the WGSL text is the cache key).
pub fn standalone_for_spec<P: crate::primitive::PrimitiveSpec>(
) -> Result<String, CodegenError> {
    let body = P::WGSL_BODY.ok_or(CodegenError::NoBody)?;
    let spec = StandaloneKernelSpec {
        fusion_kind: P::FUSION_KIND,
        body,
        inputs: P::INPUTS,
        params: P::PARAMS,
        input_access: P::INPUT_ACCESS,
        derived_uniforms: P::DERIVED_UNIFORMS,
        outputs: P::OUTPUTS,
        stencil_fetch: P::STENCIL_FETCH,
        includes: P::WGSL_INCLUDES,
    };
    // Buffer atoms (Array output) route directly so they can carry
    // `DERIVED_UNIFORMS` (frame-derived non-param uniform fields). The texture
    // path's public `generate_standalone` signature stays untouched.
    if P::OUTPUTS.iter().any(|o| matches!(o.ty, PortType::Array(_))) {
        return generate_standalone_buffer_with_options(
            &spec,
            P::ATOMIC_OUTPUTS,
            P::OWNED_OUTPUTS,
            P::BUFFER_INDEX,
        );
    }
    // Resolve kernels cannot read textures. Texture-domain atoms with Array
    // inputs use the standalone path's BufferIndex support instead.
    if P::INPUTS.iter().any(|i| matches!(i.ty, PortType::Array(_)))
        && !P::INPUTS.iter().any(is_texture_input)
    {
        return generate_standalone_resolve(body, P::INPUTS, P::PARAMS, P::OUTPUTS);
    }
    generate_standalone(&spec)
}

/// Boundary atoms need an emission shape because their declared fusion kind
/// does not describe texture-input arity.
fn emission_shape_for(
    inputs: &[crate::ports::NodePort],
) -> crate::freeze::classify::FusionKind {
    use crate::freeze::classify::FusionKind;
    match inputs.iter().filter(|i| is_texture_input(i)).count() {
        0 => FusionKind::Source,
        1 => FusionKind::Pointwise,
        _ => FusionKind::MultiInputCoincident,
    }
}

/// Standalone codegen for an atom whose declared fusion kind is `Boundary`
/// (fusion-exempt per `docs/ADDING_PRIMITIVES.md`'s taxonomy) but whose
/// runtime kernel is still GENERATED from its `wgsl_body` — the atom is
/// excused from fusion, not from the codegen path. First inhabitant:
/// `node.bokeh_gather` (`BoundaryReason::BarrieredReduction` — its internal
/// prefilter mip chain is a barriered multi-pass dependency the fused form
/// can never express, so the body samples the raw `tex_*` bindings at a
/// computed LOD; legal because a Boundary atom's body is only ever emitted
/// standalone, where those bindings exist).
///
/// Guards against misuse on fusable atoms: `standalone_for_spec` is the
/// entry for anything that can fuse.
pub fn standalone_for_boundary_spec<P: crate::primitive::PrimitiveSpec>(
) -> Result<String, CodegenError> {
    use crate::freeze::classify::FusionKind;
    assert_eq!(
        P::FUSION_KIND,
        FusionKind::Boundary,
        "standalone_for_boundary_spec is for Boundary atoms; fusable atoms use standalone_for_spec",
    );
    let body = P::WGSL_BODY.ok_or(CodegenError::NoBody)?;
    let spec = StandaloneKernelSpec {
        fusion_kind: emission_shape_for(P::INPUTS),
        body,
        inputs: P::INPUTS,
        params: P::PARAMS,
        input_access: P::INPUT_ACCESS,
        derived_uniforms: P::DERIVED_UNIFORMS,
        outputs: P::OUTPUTS,
        stencil_fetch: P::STENCIL_FETCH,
        includes: P::WGSL_INCLUDES,
    };
    generate_standalone(&spec)
}

/// Like [`standalone_for_spec`] but emits the output storage texture at `fmt`
/// instead of the hardcoded rgba16float. The unfused side of FULL-PRECISION
/// in-loop fusion: a texture atom inside a chaotic feedback loop can declare an
/// fp32 output (via `outputFormats`), and then the editor (unfused) stores its
/// intermediates exactly — matching the fused kernel's f32 registers, so fused ==
/// unfused. A targeted replace of the single dst binding token is safe: input
/// textures are `texture_2d<f32>` (no storage format), so `<rgba16float, write>`
/// appears only on the output. Non-fp32 (incl. the f16 default) returns unchanged.
pub fn standalone_for_spec_fmt<P: crate::primitive::PrimitiveSpec>(
    fmt: manifold_gpu::GpuTextureFormat,
) -> Result<String, CodegenError> {
    let wgsl = standalone_for_spec::<P>()?;
    let Some(token) = wgsl_storage_token(fmt) else {
        return Ok(wgsl); // unknown / unsupported → leave the f16 default
    };
    if token == "rgba16float" {
        return Ok(wgsl);
    }
    Ok(wgsl
        .replace(
            "texture_storage_2d<rgba16float, write>",
            &format!("texture_storage_2d<{token}, write>"),
        )
        .replace(
            "texture_storage_3d<rgba16float, write>",
            &format!("texture_storage_3d<{token}, write>"),
        ))
}

/// Dynamic mirror of [`standalone_for_spec`] — generates the same standalone
/// kernel text, but reads the atom's const metadata through the type-erased
/// [`EffectNode`](crate::node_graph::effect_node::EffectNode) trait instead of
/// a compile-time `PrimitiveSpec` type parameter.
///
/// Registry-driven prewarming only has type-erased nodes, so it cannot call
/// the generic entry point.
///
/// Returns `Err(CodegenError::NoBody)` for any node with no `wgsl_body` (hand-
/// written pipelines like `render_scene`/`gltf_texture_source`/`draw_*`, and
/// `wgsl_compute`'s user-authored kernels) — callers should treat that as
/// "nothing to prewarm here", not a failure.
pub fn standalone_for_node(
    node: &dyn crate::exec::effect_node::EffectNode,
) -> Result<String, CodegenError> {
    let body = node.wgsl_body().ok_or(CodegenError::NoBody)?;
    let mut spec = StandaloneKernelSpec {
        fusion_kind: node.fusion_kind(),
        body,
        inputs: node.inputs(),
        params: node.parameters(),
        input_access: node.input_access(),
        derived_uniforms: node.derived_uniforms(),
        outputs: node.outputs(),
        stencil_fetch: node.stencil_fetch(),
        includes: node.wgsl_includes(),
    };
    if node.outputs().iter().any(|o| matches!(o.ty, PortType::Array(_))) {
        return generate_standalone_buffer_with_options(
            &spec,
            node.atomic_outputs(),
            node.owned_outputs(),
            node.buffer_index(),
        );
    }
    if node.inputs().iter().any(|i| matches!(i.ty, PortType::Array(_)))
        && !node.inputs().iter().any(is_texture_input)
    {
        return generate_standalone_resolve(body, node.inputs(), node.parameters(), node.outputs());
    }
    // Fusion-exempt (Boundary) texture atoms still get their standalone kernel
    // generated — the emission shape comes from the ports, not the declared
    // kind (dynamic mirror of `standalone_for_boundary_spec`; first inhabitant
    // `node.bokeh_gather`, kept prewarmed through this path).
    if node.fusion_kind() == crate::freeze::classify::FusionKind::Boundary {
        spec.fusion_kind = emission_shape_for(node.inputs());
    }
    generate_standalone(&spec)
}
