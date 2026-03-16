// Resolve atomic deposit accumulator into trail texture.
// Reads existing trail from trail_read, adds deposit from accum buffer,
// writes to trail_write, and clears the accumulator.

struct ResolveUniforms {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var trail_read: texture_2d<f32>;
@group(0) @binding(1) var trail_write: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<storage, read_write> accum: array<atomic<u32>>;
@group(0) @binding(3) var<uniform> params: ResolveUniforms;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.width || id.y >= params.height {
        return;
    }

    let existing = textureLoad(trail_read, vec2<i32>(i32(id.x), i32(id.y)), 0).r;
    let idx = id.y * params.width + id.x;
    let val = atomicLoad(&accum[idx]);
    let deposit = f32(val) / 4096.0;

    textureStore(trail_write, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(existing + deposit, 0.0, 0.0, 1.0));

    atomicStore(&accum[idx], 0u);
}
