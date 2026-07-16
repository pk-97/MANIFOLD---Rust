// node.skin_mesh — fusable BUFFER body (freeze §12, buffer domain).
// Per-vertex linear-blend GPU skinning (glTF spec formula): blend up to 4
// joint matrices per vertex, looked up from buf_matrices (BufferGather —
// not coincident with the per-vertex dispatch) by the coincident
// per-vertex e_joints/e_weights. Weights normalized defensively. No
// legacy hand-WGSL predecessor exists for this brand-new primitive — the
// gpu_tests parity oracle is an independently-implemented Rust reference
// of this exact formula (DECOMPOSING_GENERATORS.md §9), not a parallel
// .wgsl file.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_joints: Element2,
    e_weights: Element2,
    joint_count: i32,
    joints_len: u32,
    weights_len: u32,
    matrices_len: u32,
) -> Element {
    var pos = vec3<f32>(0.0, 0.0, 0.0);
    var nrm = vec3<f32>(0.0, 0.0, 0.0);
    let jc = array<f32, 4>(e_joints.x, e_joints.y, e_joints.z, e_joints.w);
    let wc = array<f32, 4>(e_weights.x, e_weights.y, e_weights.z, e_weights.w);
    let wsum = max(wc[0] + wc[1] + wc[2] + wc[3], 1e-8);
    let max_j = max(matrices_len, 1u) - 1u;
    for (var k = 0u; k < 4u; k = k + 1u) {
        let w = wc[k] / wsum;
        let j = min(u32(round(jc[k])), max_j);
        let m = buf_matrices[j];
        let tp = m.mat_col0.xyz * e_in.position.x + m.mat_col1.xyz * e_in.position.y
            + m.mat_col2.xyz * e_in.position.z + m.mat_col3.xyz;
        let tn = m.mat_col0.xyz * e_in.normal.x + m.mat_col1.xyz * e_in.normal.y
            + m.mat_col2.xyz * e_in.normal.z;
        pos += w * tp;
        nrm += w * tn;
    }
    let mag = max(length(nrm), 1e-12);
    return Element(pos, nrm / mag, e_in.uv);
}
