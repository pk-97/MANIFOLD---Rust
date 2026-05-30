// node.block_displace_field — per-block random UV-offset field.
// Pure generator (reads its own dims). Emits two textures:
//   offset_out : RG = signed per-block UV displacement (x, y), gated so
//                only a fraction of blocks move (B=0, A=1). Feed into
//                node.remap (Relative mode), alone or summed with other
//                offset fields.
//   hash_out   : R = the raw per-block hash in [0,1) (same hash the gate
//                uses) for downstream per-block accents (e.g. invert).
//
// Block-displace math is verbatim from the old fused fx_glitch /
// node.glitch_displace; the scanline jitter and per-block invert it
// fused alongside are now separate nodes (node.scanline_jitter_field,
// node.invert + node.masked_mix on the thresholded hash).

struct Uniforms {
    amount: f32,
    block_size: f32,
    speed: f32,
    time: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var offset_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var hash_out: texture_storage_2d<rgba16float, write>;

fn hash2(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(offset_out);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let res = vec2<f32>(dims);
    let t = floor(u.time * u.speed * 12.0);

    let block_pixels = max(u.block_size, 4.0);
    let block_uv = floor(uv * res / block_pixels);
    let block_hash = hash2(block_uv + t * 0.37);

    let displace_mask = step(1.0 - u.amount * 0.6, block_hash);
    let displace_x = (hash2(block_uv + t * 1.13) * 2.0 - 1.0) * u.amount * 0.15;
    let displace_y = (hash2(block_uv + t * 2.77) * 2.0 - 1.0) * u.amount * 0.03;
    let offset = vec2<f32>(displace_x, displace_y) * displace_mask;

    textureStore(offset_out, vec2<i32>(id.xy), vec4<f32>(offset.x, offset.y, 0.0, 1.0));
    textureStore(hash_out, vec2<i32>(id.xy), vec4<f32>(block_hash, block_hash, block_hash, 1.0));
}
