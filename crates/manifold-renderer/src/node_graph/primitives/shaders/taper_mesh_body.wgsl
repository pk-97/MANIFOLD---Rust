// node.taper_mesh — fusable BUFFER body (freeze §12, buffer domain),
// COINCIDENT `in` + COINCIDENT optional `weights`. Per-vertex taper along
// `axis`: the two off-axis position components scale by s; normals divide
// by s and renormalize (D4 exact for this transform). Matches
// taper_mesh.wgsl.
//
// s(v) = mix(1, taper, clamp((coord(v) - center) / length, 0, 1) * w) (w =
// weights[idx] or 1.0 past weights_len, D2 degrade-to-1.0). `t*w` is NOT
// re-clamped after the mix — honest extrapolation for weights above 1.0.
//
// The 4th positional param below is the design's `length` param — renamed
// to `p_length` in the GENERATED uniform struct only (freeze/codegen.rs
// RESERVED list: `length` collides with the WGSL length() builtin); this
// hand fragment names its own positional parameter `taper_length` to sidestep
// the identifier entirely regardless of the generated field name.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_weights: f32,
    axis: u32,
    taper: f32,
    center: f32,
    taper_length: f32,
    weights_len: u32,
) -> Element {
    let w = select(1.0, e_weights, idx < weights_len);

    var coord: f32;
    if axis == 0u {
        coord = e_in.position.x;
    } else if axis == 1u {
        coord = e_in.position.y;
    } else {
        coord = e_in.position.z;
    }
    let len_safe = max(taper_length, 1e-6);
    let t = clamp((coord - center) / len_safe, 0.0, 1.0);
    let s = mix(1.0, taper, t * w);
    let denom = select(s, 1e-6, abs(s) < 1e-6);

    var pos = e_in.position;
    var nrm = e_in.normal;
    if axis == 0u {
        pos.y = pos.y * s;
        pos.z = pos.z * s;
        nrm.y = nrm.y / denom;
        nrm.z = nrm.z / denom;
    } else if axis == 1u {
        pos.z = pos.z * s;
        pos.x = pos.x * s;
        nrm.z = nrm.z / denom;
        nrm.x = nrm.x / denom;
    } else {
        pos.x = pos.x * s;
        pos.y = pos.y * s;
        nrm.x = nrm.x / denom;
        nrm.y = nrm.y / denom;
    }
    nrm = normalize(nrm);

    return Element(pos, nrm, e_in.uv);
}
