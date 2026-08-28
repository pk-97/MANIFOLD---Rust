// node.bokeh_gather — internal mip-chain builder (the prefilter half of the
// silhouette-speckle fix; see bokeh_gather_body.wgsl's header). One dispatch
// per mip level: each dst texel reads its 2x2 footprint in the source level
// with ONE bilinear fetch at the footprint center (weights exactly 0.25 —
// an exact box average for even dims, and a well-defined clamped sample for
// odd dims). Hand-authored instead of GpuEncoder::generate_mipmaps so the
// filter is exact by construction on every backend — Metal's blit mipgen
// filter is undocumented, and the I1 CPU-reference parity proof must model
// the filter precisely.
//
// Level 0 of the chain is filled by this same kernel with `src` bound to the
// atom's original `in` texture (identity UV → bilinear at texel centers →
// the texel itself), which also normalizes any float source format into the
// chain's rgba16float. The chain texture and its per-level views are cached
// by run() and rebuilt only on resize.
//
// Bindings: src(0, single-level view or the full-res input), samp(1, default
// linear), dst(2, single-level storage view of the chain at the target mip).

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims_i = textureDimensions(dst);
    if id.x >= dims_i.x || id.y >= dims_i.y {
        return;
    }
    let dims = vec2<f32>(f32(dims_i.x), f32(dims_i.y));
    let uv = (vec2<f32>(f32(id.x), f32(id.y)) + vec2<f32>(0.5)) / dims;
    textureStore(dst, vec2<i32>(id.xy), textureSampleLevel(src, samp, uv, 0.0));
}
