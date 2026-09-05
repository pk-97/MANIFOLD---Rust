// node.reflect_array — fusable BUFFER body (SCENE_MIRROR design D3/D4/D5).
// Whole-array planar reflection of Array<InstanceTransform>: output slot j
// in [0, cap) is original instance j; slot cap + j is its reflection across
// the plane perpendicular to `axis` through d = plane_offset along it. The
// output capacity is FIXED at 2x the input capacity at plan pre-allocation —
// axis / plane_offset / enabled are live card writes and never trigger a
// rebuild (the BUG-757c capacity rule).
//
// ABI (buffer standalone codegen): the `in` port is a BufferGather input —
// this body indexes buf_in itself (slot idx % cap; a coincident pre-read
// would run off the end of the smaller input array). GATHER form, so the
// atom is a fusion boundary today (same as node.neighbor_smooth): the fused
// buffer wrapper keys its dispatch on the INPUT array length and cannot
// express a 2x-capacity output. Standalone-only until the compiler grows
// that expression — tracked debt, not a quiet exemption.
//
// Marker (D5): rot.w = mirror plane component + 1 on mirrored slots
// (1 = x, 2 = y, 3 = z), 0 on originals. The plane is the same for +axis
// and -axis (M = I - 2 a a^T is sign-blind), so the marker carries the
// plane COMPONENT, not the signed axis; render_scene.wgsl's vertex stage
// flips that component of the vertex position and normal BEFORE applying
// the stored rotation, giving the exact mirrored point R'(w M v) + p' and
// normal M R n (R' M = M R).
//
// Euler conjugation, derived against render_scene.wgsl's euler_xyz
// (R = Rz . Ry . Rx, column vectors, x-angle applied first). For
// M = diag(m_x, m_y, m_z): M Rz M = Rz(m_x m_y z), M Ry M = Ry(m_x m_z y),
// M Rx M = Rx(m_y m_z x). With the plane at component c (m_c = -1, the
// other two +1) the angle about the mirror normal keeps its sign and the
// two about in-plane axes negate.
fn body(idx: u32, count: u32, axis: u32, plane_offset: f32, enabled: f32) -> Element {
    let in_cap = arrayLength(&buf_in);
    if (in_cap == 0u) {
        return Element(vec4<f32>(0.0), vec4<f32>(0.0));
    }
    let src = buf_in[idx % in_cap];
    let live = src.pos_scale.w != 0.0;

    // Originals half. `enabled` does NOT gate this half — off is free
    // (INV-MR1/D9): originals pass through while the source slot is live;
    // a dead (zero-scale) source stays zero-scale so the whole-buffer
    // instance draw rasterizes nothing for it.
    if (idx < in_cap) {
        if (!live) {
            return Element(vec4<f32>(0.0), vec4<f32>(0.0));
        }
        // Marker discipline (INV-MR3): originals always carry marker 0.
        return Element(src.pos_scale, vec4<f32>(src.rot.xyz, 0.0));
    }

    // Mirrored half (INV-MR5): exists iff the source slot is live AND the
    // gate is on (D9).
    if (!live || enabled == 0.0) {
        return Element(vec4<f32>(0.0), vec4<f32>(0.0));
    }

    let comp = axis / 2u; // plane component 0/1/2
    let s = select(1.0, -1.0, (axis & 1u) == 1u); // signed axis direction

    // pos' = M (pos - d a) + d a — component `comp` becomes
    // -(pos - d s) + d s = -pos + 2 d s; the other two pass through.
    var p = src.pos_scale.xyz;
    if (comp == 0u) { p.x = -p.x + 2.0 * plane_offset * s; }
    else if (comp == 1u) { p.y = -p.y + 2.0 * plane_offset * s; }
    else { p.z = -p.z + 2.0 * plane_offset * s; }

    // R' = M R M as the Euler sign map (header derivation): the angle about
    // the mirror normal keeps its sign, the two about in-plane axes negate.
    var r = src.rot.xyz;
    if (comp == 0u) { r = vec3<f32>(r.x, -r.y, -r.z); }
    else if (comp == 1u) { r = vec3<f32>(-r.x, r.y, -r.z); }
    else { r = vec3<f32>(-r.x, -r.y, r.z); }

    // Scale stays positive (D5 as amended at P1 adjudication): the mirror
    // flip rides on the vertex in render_scene.wgsl, not on the scale —
    // a negated uniform scale is a central inversion, not a reflection.
    // The marker is the plane component + 1.
    return Element(vec4<f32>(p, src.pos_scale.w), vec4<f32>(r, f32(comp + 1u)));
}
