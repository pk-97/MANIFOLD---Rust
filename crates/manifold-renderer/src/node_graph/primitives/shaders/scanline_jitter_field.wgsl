// node.scanline_jitter_field — per-row random horizontal-offset field.
// Pure generator (reads its own dims). Emits one texture:
//   offset_out : R = signed horizontal UV shift per scanline row, gated
//                so only a fraction of rows tear (G=B=0, A=1). Feed into
//                node.remap (Relative mode), alone or summed with other
//                offset fields.
//
// Scanline-jitter math is verbatim from the old fused fx_glitch /
// node.glitch_displace, except the row index is taken from the original
// uv.y (the fused pass took it from the block-displaced uv.y — a weak
// coupling now dropped so each field is a pure function of the source UV).

struct Uniforms {
    amount: f32,
    scanline: f32,
    speed: f32,
    time: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var offset_out: texture_storage_2d<rgba16float, write>;

fn hash1(n: f32) -> f32 {
    return fract(sin(n) * 43758.5453123);
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

    let scanline_row = floor(uv.y * res.y);
    let scan_hash = hash1(scanline_row + t * 7.31);
    let scan_mask = step(1.0 - u.scanline * u.amount * 0.3, scan_hash);
    let scan_shift = (hash1(scanline_row + t * 3.17) * 2.0 - 1.0) * u.amount * 0.08;
    let offset_x = scan_shift * scan_mask;

    textureStore(offset_out, vec2<i32>(id.xy), vec4<f32>(offset_x, 0.0, 0.0, 1.0));
}
