// node.glitch_displace — the motion + accent field of a digital glitch.
// Pure generator (reads its own dims). Emits two textures:
//   uv_out   : RG = the per-pixel sampling UV after block displacement
//              + scanline jitter (B=0, A=1). Feed into node.remap.
//   mask_out : R = the per-block invert accent mask (0 or 1), for a
//              downstream masked invert.
// Block-displace + scanline-jitter math is verbatim from fx_glitch /
// node.glitch; the chromatic split and per-block invert that the legacy
// fused into the same pass are now composed downstream (chromatic_
// aberration + invert + masked_mix). Time drives the random hash.

struct Uniforms {
    amount: f32,
    block_size: f32,
    scanline: f32,
    speed: f32,
    time: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var uv_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var mask_out: texture_storage_2d<rgba16float, write>;

fn hash1(n: f32) -> f32 {
    return fract(sin(n) * 43758.5453123);
}

fn hash2(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(uv_out);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }
    let uv_orig = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    var uv = uv_orig;
    let res = vec2<f32>(dims);
    let t = floor(u.time * u.speed * 12.0);

    let block_pixels = max(u.block_size, 4.0);
    let block_uv = floor(uv * res / block_pixels);
    let block_hash = hash2(block_uv + t * 0.37);

    let displace_mask = step(1.0 - u.amount * 0.6, block_hash);
    let displace_x = (hash2(block_uv + t * 1.13) * 2.0 - 1.0) * u.amount * 0.15;
    let displace_y = (hash2(block_uv + t * 2.77) * 2.0 - 1.0) * u.amount * 0.03;
    uv = uv + vec2<f32>(displace_x, displace_y) * displace_mask;

    let scanline_row = floor(uv.y * res.y);
    let scan_hash = hash1(scanline_row + t * 7.31);
    let scan_mask = step(1.0 - u.scanline * u.amount * 0.3, scan_hash);
    let scan_shift = (hash1(scanline_row + t * 3.17) * 2.0 - 1.0) * u.amount * 0.08;
    uv.x = uv.x + scan_shift * scan_mask;

    let invert_mask = step(0.92, block_hash * u.amount);

    textureStore(uv_out, vec2<i32>(id.xy), vec4<f32>(uv.x, uv.y, 0.0, 1.0));
    textureStore(mask_out, vec2<i32>(id.xy), vec4<f32>(invert_mask, invert_mask, invert_mask, 1.0));
}
