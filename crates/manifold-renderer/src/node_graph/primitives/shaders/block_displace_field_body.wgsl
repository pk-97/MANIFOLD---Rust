// node.block_displace_field — fusable body (freeze §12), SOURCE + MULTI-OUTPUT.
// Per-block random UV-offset field (datamosh/block-glitch). Quantises the canvas
// into block_size-px blocks, hashes each (animated by `time`), emits `offset`
// (RG signed displacement gated so only a fraction of blocks move) and `hash` (R
// raw per-block hash). The body returns both in BodyOutputs (the codegen-declared
// struct); res comes from the ambient dims. Matches block_displace_field.wgsl.
// PARAMS: [amount, block_size, speed, time] (+ injected write_offset/write_hash).
fn bdf_hash2(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}

fn body(uv: vec2<f32>, dims: vec2<f32>, amount: f32, block_size: f32, speed: f32, time: f32) -> BodyOutputs {
    let res = dims;
    let t = floor(time * speed * 12.0);

    let block_pixels = max(block_size, 4.0);
    let block_uv = floor(uv * res / block_pixels);
    let block_hash = bdf_hash2(block_uv + t * 0.37);

    let displace_mask = step(1.0 - amount * 0.6, block_hash);
    let displace_x = (bdf_hash2(block_uv + t * 1.13) * 2.0 - 1.0) * amount * 0.15;
    let displace_y = (bdf_hash2(block_uv + t * 2.77) * 2.0 - 1.0) * amount * 0.03;
    let offset = vec2<f32>(displace_x, displace_y) * displace_mask;

    return BodyOutputs(
        vec4<f32>(offset.x, offset.y, 0.0, 1.0),
        vec4<f32>(block_hash, block_hash, block_hash, 1.0),
    );
}
