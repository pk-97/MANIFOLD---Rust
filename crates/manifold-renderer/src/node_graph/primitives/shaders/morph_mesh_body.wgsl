// node.morph_mesh — fusable BUFFER body (freeze §12, buffer domain),
// COINCIDENT 2-mesh-input (the node.blend_copies shape, MeshVertex instead
// of InstanceTransform) + COINCIDENT optional `weights`. Static two-mesh
// lerp by index: pos = mix(a, b, t*w), normal = normalize(mix(a.n, b.n,
// t*w)), uv from `a`. Matches morph_mesh.wgsl. Uses the SAME explicit
// `a + (b-a)*x` form (not mix()) is NOT required here since both kernels
// share this one body verbatim — parity is definitional, not a
// re-implementation risk.
//
// ABI: e_in/e_b are coincident pre-reads of the two mesh inputs; e_weights
// is the coincident pre-read of the optional weights buffer (w = 1.0 past
// weights_len, D2 degrade-to-1.0, never silent 0).
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_b: Element,
    e_weights: f32,
    t: f32,
    weights_len: u32,
) -> Element {
    let w = select(1.0, e_weights, idx < weights_len);
    let tw = t * w;

    let pos = mix(e_in.position, e_b.position, tw);
    let n = mix(e_in.normal, e_b.normal, tw);
    let mag = max(length(n), 1e-12);

    return Element(pos, n / mag, e_in.uv);
}
