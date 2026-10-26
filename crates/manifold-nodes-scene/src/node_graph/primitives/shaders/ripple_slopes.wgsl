// Surface slopes of a tileable height field, for the water's ripple detail.
// `heights` is N rows (z) of N columns (x), row-major, x fastest. The slopes
// are central differences with wrapped neighbours, so the tile repeats without
// a seam. Units: metres of height per metre.

struct SlopeUniforms {
    n: u32,
    texel_metres: f32,
    _pad: vec2<u32>,
};

@group(0) @binding(0) var<uniform> params: SlopeUniforms;
@group(0) @binding(1) var<storage, read> heights: array<f32>;
@group(0) @binding(2) var slopes: texture_storage_2d<rg16float, write>;

fn height_at(x: u32, z: u32) -> f32 {
    return heights[z * params.n + x];
}

@compute @workgroup_size(16, 16)
fn cs_slopes(@builtin(global_invocation_id) id: vec3<u32>) {
    let n = params.n;
    if id.x >= n || id.y >= n {
        return;
    }
    let xm = (id.x + n - 1u) % n;
    let xp = (id.x + 1u) % n;
    let zm = (id.y + n - 1u) % n;
    let zp = (id.y + 1u) % n;
    let inv = 1.0 / (2.0 * params.texel_metres);
    let sx = (height_at(xp, id.y) - height_at(xm, id.y)) * inv;
    let sz = (height_at(id.x, zp) - height_at(id.x, zm)) * inv;
    textureStore(slopes, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(sx, sz, 0.0, 0.0));
}
