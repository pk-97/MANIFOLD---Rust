// node.scene_array — fusable BUFFER body (freeze section 12, buffer domain),
// POINTWISE per output slot. Windowed modulo-tiled corridor
// (SCENE_LOOP_ENDLESS_CORRIDOR_DESIGN.md D1/D4/D5): slot w ↔ corridor cell
// c = base_cell − behind + w, transform = translation c·cell_size along axis
// plus jitter keyed on the Euclidean (c mod pattern_length). Slots at or beyond
// behind+ahead+1 are surplus capacity — zero-scale, same mask as BUG-757c.
//
// ABI (buffer standalone codegen): no array inputs, so the body takes
// (idx, count, <params...>, <derived uniforms...>) and returns the output
// element written to buf_out[idx]. The codegen synthesizes
//   struct Element { pos_scale: vec4<f32>, rot: vec4<f32> }
// from InstanceTransform's Channels signature. `dispatch_count` (= the OUTPUT
// capacity, always WINDOW_CAPACITY) is the wrapper guard. `hash_u32` comes from
// noise_common.wgsl, prepended via wgsl_includes.
//
// Axis encoding: 0=+X, 1=-X, 2=+Y, 3=-Y, 4=+Z, 5=-Z.
// Int params arrive as i32, Enum as u32, Float as f32.
// Derived uniforms (run() resolves them CPU-side from the wired camera,
// exactly like node.flatten_3d's camera seam): base_cell(i32), behind(u32),
// ahead(u32), use_camera(u32). use_camera is stasis-key only — run() already
// folds the unwired default (base_cell 0, ahead 22) into base_cell/ahead, so
// the body branches on nothing the uniforms don't already carry.

fn body(
    idx: u32,
    count: u32,
    pattern_length: i32,
    axis: u32,
    cell_size: f32,
    jitter_seed: i32,
    jitter_amount: f32,
    base_cell: i32,
    behind: u32,
    ahead: u32,
    use_camera: u32,
) -> Element {
    // Window mask: live slots are w < behind+ahead+1 (BUG-757c mask, corridor
    // edition — the window span replaces the old live count). Zero scale
    // collapses the instance's vertices to a point; degenerate triangles
    // rasterize nothing, in the main pass and the shadow passes alike.
    if idx >= behind + ahead + 1u {
        return Element(vec4<f32>(0.0, 0.0, 0.0, 0.0), vec4<f32>(0.0));
    }

    let c = base_cell - i32(behind) + i32(idx);
    let t = f32(c) * cell_size;
    var pos = vec3<f32>(0.0, 0.0, 0.0);
    if axis == 0u { pos.x = t; }
    else if axis == 1u { pos.x = -t; }
    else if axis == 2u { pos.y = t; }
    else if axis == 3u { pos.y = -t; }
    else if axis == 4u { pos.z = t; }
    else { pos.z = -t; }

    // Per-instance jitter: rotation (radians, ±jitter_amount per axis) and
    // scale (1 ± jitter_amount/2) from hash_u32 keyed by (c mod pattern_length)
    // mixed with the seed. The mod MUST be Euclidean on the signed cell index:
    // WGSL % truncates toward zero, which is not P-periodic across cell zero —
    // cell −1 would hash as 0xFFFFFFFF instead of P−1 and the camera's own
    // cell would carry different jitter across the wrap (D4, review finding 1;
    // the CPU oracle mirrors this with rem_euclid).
    var rot = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    var scl = 1.0;
    if jitter_amount > 0.0 {
        let s = u32(jitter_seed);
        let p = max(pattern_length, 1);
        let m = ((c % p) + p) % p;
        let j = u32(m);
        let k = j * 3u + s * 7919u;
        rot = vec4<f32>(
            (hash_u32(k)      - 0.5) * 2.0 * jitter_amount,
            (hash_u32(k + 1u) - 0.5) * 2.0 * jitter_amount,
            (hash_u32(k + 2u) - 0.5) * 2.0 * jitter_amount,
            0.0,
        );
        scl = 1.0 + (hash_u32(k + 3u) - 0.5) * jitter_amount;
    }
    return Element(vec4<f32>(pos, scl), rot);
}
