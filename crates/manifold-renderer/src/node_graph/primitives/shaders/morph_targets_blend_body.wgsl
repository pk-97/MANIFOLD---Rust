// node.morph_targets_blend — fusable BUFFER body (freeze §12, buffer domain).
// glTF additive morph-target blend: out = base + sum(weight[t] * delta[t])
// over up to `target_count` targets, deltas looked up from buf_deltas
// (BufferGather, flattened target-major: deltas[t * count + idx] — NOT
// coincident with the per-vertex dispatch) and weights from buf_weights
// (BufferGather, one f32 per target — node.gltf_morph_weights' output).
// No legacy hand-WGSL predecessor exists for this brand-new primitive — the
// gpu_tests parity oracle is an independently-implemented Rust reference
// of this exact formula (DECOMPOSING_GENERATORS.md §9), not a parallel
// .wgsl file.
//
// The effective loop bound is `min(target_count, weights_len, deltas_len /
// count)` — a short or mismatched deltas/weights buffer truncates (skips
// the missing targets) rather than reading out of bounds (GLTF_ANIMATION_
// DESIGN.md A3 buffer-length hazard). `target_count == 0` (or an empty
// bound) is a strict base pass-through.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    target_count: i32,
    deltas_len: u32,
    weights_len: u32,
) -> Element {
    var pos = e_in.position;
    var nrm = e_in.normal;

    let tc = u32(max(target_count, 0));
    let by_deltas = select(0u, deltas_len / max(count, 1u), count > 0u);
    let bound = min(tc, min(weights_len, by_deltas));

    for (var t = 0u; t < bound; t = t + 1u) {
        let w = buf_weights[t];
        let d = buf_deltas[t * count + idx];
        pos = pos + w * d.position;
        nrm = nrm + w * d.normal;
    }

    let mag = max(length(nrm), 1e-12);
    return Element(pos, nrm / mag, e_in.uv);
}
