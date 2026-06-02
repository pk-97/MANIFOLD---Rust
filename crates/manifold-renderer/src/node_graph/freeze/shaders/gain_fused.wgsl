// Hand-fused reference for a Source -> Gain x N -> FinalOutput chain.
//
// The unfused chain rounds its RGB to f16 in the storage texture after EVERY
// Gain pass; this kernel folds the whole chain to a single multiply by the
// product of the gains, kept in f32 registers and rounded to f16 exactly once
// on write. So fused != unfused bit-exact — they diff by the accumulated
// intermediate f16 rounding, which is precisely the divergence the oracle's
// two-sided tolerance is meant to absorb (design §11.D). Read once, multiply,
// write once: the bandwidth collapse the real compiler will reproduce.
//
// Reads the source with textureLoad (exact texel) rather than Gain's
// center-UV sample — identical result for same-dimension textures, no sampler.

struct U {
    product: f32,
    _p0: f32,
    _p1: f32,
    _p2: f32,
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(dst);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let c = vec2<i32>(i32(id.x), i32(id.y));
    let s = textureLoad(src, c, 0);
    textureStore(dst, c, vec4<f32>(s.rgb * u.product, s.a));
}
