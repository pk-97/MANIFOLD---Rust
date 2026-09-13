// node.morph_mesh — fusable BUFFER body (freeze section 12, buffer domain),
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
// weights_len, D2 degrade-to-1.0, never silent 0). `blend_frames` is a Bool
// parameter represented as u32 by codegen. Its opt-in path interpolates
// approximate surface frames for same-topology variants.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_b: Element,
    e_weights: f32,
    t: f32,
    blend_frames: u32,
    weights_len: u32,
) -> Element {
    let w = select(1.0, e_weights, idx < weights_len);
    let raw_tw = t * w;

    // Keep the legacy path byte-for-byte in its semantic fields when the
    // opt-in parameter is at its default.
    if blend_frames == 0u {
        let pos = mix(e_in.position, e_b.position, raw_tw);
        let n = mix(e_in.normal, e_b.normal, raw_tw);
        let mag = max(length(n), 1e-12);
        return Element(pos, n / mag, e_in.uv, e_in.tangent);
    }

    let tw = clamp(raw_tw, 0.0, 1.0);

    // Exact endpoint rules make masking predictable and preserve the input
    // vertex at zero weight. UVs always remain sourced from the input mesh.
    if tw <= 0.0 {
        return e_in;
    }
    if tw >= 1.0 {
        return Element(e_b.position, e_b.normal, e_in.uv, e_b.tangent);
    }

    let mixed_normal = mix(e_in.normal, e_b.normal, tw);
    let normal_length = length(mixed_normal);
    var normal = vec3<f32>(0.0, 1.0, 0.0);
    if normal_length > 1e-12 {
        normal = mixed_normal / normal_length;
    } else {
        let input_length = length(e_in.normal);
        if input_length > 1e-12 {
            normal = e_in.normal / input_length;
        } else {
            let target_length = length(e_b.normal);
            if target_length > 1e-12 {
                normal = e_b.normal / target_length;
            }
        }
    }

    let mixed_tangent = mix(e_in.tangent.xyz, e_b.tangent.xyz, tw);
    let tangent_length = length(mixed_tangent);
    var tangent_xyz = vec3<f32>(0.0, 0.0, 0.0);
    if tangent_length > 1e-12 {
        let unit_tangent = mixed_tangent / tangent_length;
        let orthogonal = unit_tangent - normal * dot(unit_tangent, normal);
        let orthogonal_length = length(orthogonal);
        if orthogonal_length > 1e-12 {
            tangent_xyz = orthogonal / orthogonal_length;
        }
    }
    // At the exact midpoint the input endpoint wins the tie.
    let handedness = select(e_in.tangent.w, e_b.tangent.w, tw > 0.5);
    return Element(
        mix(e_in.position, e_b.position, tw),
        normal,
        e_in.uv,
        vec4<f32>(tangent_xyz, handedness),
    );
}
