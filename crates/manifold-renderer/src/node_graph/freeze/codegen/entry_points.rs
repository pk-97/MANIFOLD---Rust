use crate::node_graph::ports::PortType;

use super::types::{is_texture_input, CodegenError};
use super::standalone::{
    generate_standalone, generate_standalone_buffer, generate_standalone_resolve,
    StandaloneKernelSpec,
};



/// Generate the standalone kernel for a primitive type — the single-source
/// `run()` path. Reads the body + classification + ports/params off the type's
/// `PrimitiveSpec` consts. Deterministic, so `create_compute_pipeline` caches
/// the result across instances and sessions (the WGSL text is the cache key).
pub fn standalone_for_spec<P: crate::node_graph::primitive::PrimitiveSpec>(
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
        return generate_standalone_buffer(&spec, P::ATOMIC_OUTPUTS);
    }
    // BUFFER→TEXTURE resolve: an Array input with NO texture input, feeding a
    // texture output — the accumulator-to-density bridge
    // (`generate_standalone_resolve`'s contract: exactly one atomic-integer
    // accumulator in, no texture reads at all). D3 (BUG-114) adds a SECOND,
    // distinct Array-input shape — a texture-domain atom that ALSO reads ≥1
    // texture input and tags its Array input `BufferIndex` (the `draw_*`
    // family) — which is NOT the resolve bridge and must fall through to
    // `generate_standalone` below (the codegen path that now handles
    // `BufferIndex`). Gated on "no texture input" so this branch's scope
    // stays exactly what it always was for every existing resolve atom.
    if P::INPUTS.iter().any(|i| matches!(i.ty, PortType::Array(_)))
        && !P::INPUTS.iter().any(is_texture_input)
    {
        return generate_standalone_resolve(body, P::INPUTS, P::PARAMS, P::OUTPUTS);
    }
    generate_standalone(&spec)
}

/// The emission shape `generate_standalone`'s arity gate expects, derived
/// from the texture-input count: 0 → Source, 1 → Pointwise, ≥2 →
/// MultiInputCoincident (mirrors the gate's own arms). Used only by the
/// boundary-atom entry points below, where the declared `FUSION_KIND` is
/// `Boundary` and the shape must come from the ports instead.
fn emission_shape_for(
    inputs: &[crate::node_graph::ports::NodePort],
) -> crate::node_graph::freeze::classify::FusionKind {
    use crate::node_graph::freeze::classify::FusionKind;
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
pub fn standalone_for_boundary_spec<P: crate::node_graph::primitive::PrimitiveSpec>(
) -> Result<String, CodegenError> {
    use crate::node_graph::freeze::classify::FusionKind;
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

/// WGSL storage-texture format token for the formats a texture kernel can declare
/// as a write target. `None` for anything else (the standalone path only supports
/// the f16 working default + fp32 opt-in for precision-sensitive feedback loops).
pub fn wgsl_storage_token(fmt: manifold_gpu::GpuTextureFormat) -> Option<&'static str> {
    use manifold_gpu::GpuTextureFormat as F;
    match fmt {
        F::Rgba16Float => Some("rgba16float"),
        F::Rgba32Float => Some("rgba32float"),
        _ => None,
    }
}

/// Like [`standalone_for_spec`] but emits the output storage texture at `fmt`
/// instead of the hardcoded rgba16float. The unfused side of FULL-PRECISION
/// in-loop fusion: a texture atom inside a chaotic feedback loop can declare an
/// fp32 output (via `outputFormats`), and then the editor (unfused) stores its
/// intermediates exactly — matching the fused kernel's f32 registers, so fused ==
/// unfused. A targeted replace of the single dst binding token is safe: input
/// textures are `texture_2d<f32>` (no storage format), so `<rgba16float, write>`
/// appears only on the output. Non-fp32 (incl. the f16 default) returns unchanged.
pub fn standalone_for_spec_fmt<P: crate::node_graph::primitive::PrimitiveSpec>(
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
/// `standalone_for_spec::<Self>()` needs the concrete primitive type at the
/// call site (it's generic over `P: PrimitiveSpec`), which is exactly what a
/// registry-driven prewarm sweep doesn't have — `PrimitiveRegistry::construct`
/// only ever hands back a type-erased `Box<dyn EffectNode>`. But every const
/// `standalone_for_spec` reads (`WGSL_BODY`, `INPUTS`, `OUTPUTS`, `PARAMS`,
/// `INPUT_ACCESS`, `DERIVED_UNIFORMS`, `WGSL_INCLUDES`, `ATOMIC_OUTPUTS`,
/// `FUSION_KIND`, `STENCIL_FETCH`) is ALSO exposed as a same-shaped `&dyn
/// EffectNode` method by the blanket `impl<P: Primitive> EffectNode for P`
/// (`primitive.rs`) — the trait was already carrying everything codegen
/// needs, just behind dynamic dispatch instead of a type parameter. This
/// function is that dynamic path, letting `GeneratorRegistry::prewarm_all`
/// compile every registered atom's codegen pipeline generically (BUG-146),
/// without a per-atom `prewarm_pipeline` method or a hand-maintained list.
///
/// Returns `Err(CodegenError::NoBody)` for any node with no `wgsl_body` (hand-
/// written pipelines like `render_scene`/`gltf_texture_source`/`draw_*`, and
/// `wgsl_compute`'s user-authored kernels) — callers should treat that as
/// "nothing to prewarm here", not a failure.
pub fn standalone_for_node(
    node: &dyn crate::node_graph::effect_node::EffectNode,
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
        return generate_standalone_buffer(&spec, node.atomic_outputs());
    }
    if node.inputs().iter().any(|i| matches!(i.ty, PortType::Array(_))) {
        return generate_standalone_resolve(body, node.inputs(), node.parameters(), node.outputs());
    }
    // Fusion-exempt (Boundary) texture atoms still get their standalone kernel
    // generated — the emission shape comes from the ports, not the declared
    // kind (dynamic mirror of `standalone_for_boundary_spec`; first inhabitant
    // `node.bokeh_gather`, kept prewarmed through this path).
    if node.fusion_kind() == crate::node_graph::freeze::classify::FusionKind::Boundary {
        spec.fusion_kind = emission_shape_for(node.inputs());
    }
    generate_standalone(&spec)
}
