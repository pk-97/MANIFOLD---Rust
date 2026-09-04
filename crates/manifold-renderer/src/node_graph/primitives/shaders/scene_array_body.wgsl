// node.scene_array — fusable BUFFER body (freeze section 12, buffer domain), SOURCE.
// Fill an Array<InstanceTransform> with identity TRS translated i * cell_size along axis,
// plus deterministic per-instance jitter (rotation + scale from a hash of the
// INSTANCE INDEX — no time dependence, trivially wrap-safe per SCENE_LOOP INV-3).
//
// ABI (buffer standalone codegen): no array inputs, so the body takes
// (idx, count, <params...>) and returns the output element written to
// buf_out[idx]. The codegen synthesizes
//   struct Element { pos_scale: vec4<f32>, rot: vec4<f32> }
// from InstanceTransform's Channels signature. `dispatch_count` (= the OUTPUT
// capacity) is the wrapper guard. `hash_u32` comes from noise_common.wgsl,
// prepended via wgsl_includes (same source as node.instance_rotation_jitter).
//
// Axis encoding: 0=+X, 1=-X, 2=+Y, 3=-Y, 4=+Z, 5=-Z.
// Int params arrive as i32, Enum as u32, Float as f32.

fn body(
    idx: u32,
    count: u32,
    count_param: i32,
    axis: u32,
    cell_size: f32,
    jitter_seed: i32,
    jitter_amount: f32,
) -> Element {
    let t = f32(idx) * cell_size;
    var pos = vec3<f32>(0.0, 0.0, 0.0);
    if axis == 0u { pos.x = t; }
    else if axis == 1u { pos.x = -t; }
    else if axis == 2u { pos.y = t; }
    else if axis == 3u { pos.y = -t; }
    else if axis == 4u { pos.z = t; }
    else { pos.z = -t; }

    // Per-instance jitter: rotation (radians, ±jitter_amount per axis) and
    // scale (1 ± jitter_amount/2) from hash_u32 keyed by the instance index
    // mixed with the seed. Deterministic per (idx, seed) — the same array
    // every frame, and identical at both wrap seams by construction.
    var rot = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    var scl = 1.0;
    if jitter_amount > 0.0 {
        let s = u32(jitter_seed);
        let k = idx * 3u + s * 7919u;
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
