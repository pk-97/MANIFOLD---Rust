// node.voronoi_2d — fusable body (freeze §12), SOURCE with MULTI-OUTPUT. 2D
// Worley/Voronoi cellular noise. Visits the 9 neighbouring cells of the query
// pixel and returns F1/F2 distances + a per-cell stable hash. Two outputs: `out`
// packs (F1, F2, F2-F1, cell_hash) scaled by out_scale (hash raw), `cell_id`
// carries the F1-winning cell's integer coordinate in RG. The body returns both
// in `BodyOutputs` (the codegen-declared struct, fields = output port names); the
// generated wrapper gates each store on an injected write_<port> flag. cell_point
// takes jitter as an arg (no global uniform in a body). Matches voronoi_2d.wgsl.
// PARAMS: [scale, offset_x, offset_y, jitter, out_scale].
fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn voronoi_cell_point(cell: vec2<i32>, jitter: f32) -> vec2<f32> {
    // Two independent hashes for X and Y jitter.
    let hx = wang_hash(u32(cell.x + 10000) * 73856093u ^ u32(cell.y + 10000) * 19349663u);
    let hy = wang_hash(u32(cell.x + 10000) * 83492791u ^ u32(cell.y + 10000) * 28411627u);
    let jx = f32(hx & 0xFFFFu) / 65535.0;
    let jy = f32(hy & 0xFFFFu) / 65535.0;
    return vec2<f32>(cell) + vec2<f32>(0.5, 0.5) + (vec2<f32>(jx, jy) - 0.5) * jitter;
}

fn body(uv: vec2<f32>, dims: vec2<f32>, scale: f32, offset_x: f32, offset_y: f32, jitter: f32, out_scale: f32) -> BodyOutputs {
    let p = uv * scale + vec2<f32>(offset_x, offset_y);
    let cell = vec2<i32>(floor(p));

    var f1 = 1e9;
    var f2 = 1e9;
    var f1_cell = cell;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let neighbor = cell + vec2<i32>(dx, dy);
            let fp = voronoi_cell_point(neighbor, jitter);
            let d = length(p - fp);
            if d < f1 {
                f2 = f1;
                f1 = d;
                f1_cell = neighbor;
            } else if d < f2 {
                f2 = d;
            }
        }
    }

    // Per-cell stable random — independent hash mix from the jitter hashes so
    // cell_hash decorrelates from the per-cell jitter offset.
    let cell_hash = f32(wang_hash(
        u32(f1_cell.x + 10000) * 12345701u ^ u32(f1_cell.y + 10000) * 39916801u,
    ) & 0xFFFFu) / 65535.0;

    let r = f1 * out_scale;
    let g = f2 * out_scale;
    let b = (f2 - f1) * out_scale;

    return BodyOutputs(
        vec4<f32>(r, g, b, cell_hash),
        vec4<f32>(f32(f1_cell.x), f32(f1_cell.y), 0.0, 1.0),
    );
}
