// node.mesh_ramp — fusable BUFFER body (freeze §12, buffer domain), COINCIDENT.
// Per-vertex growth-mask weight from a spatial axis sweep. The coincident input
// `in` (MeshVertex) is pre-read by the wrapper into `e_in` (Element {position,
// normal, uv}); the output is a bare f32 weight per vertex. Matches
// mesh_ramp.wgsl (the parity oracle).
//
// ABI: `axis` (Enum→u32) and `invert` (Bool→u32) are non-port-shadowed params;
// origin/phase/feather/bounds are port-shadowed f32 params run() resolves via
// scalar_or_param. No textures, no derived fields. `count` unused (DCE drops it).
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    axis: u32,
    origin_x: f32,
    origin_y: f32,
    origin_z: f32,
    phase: f32,
    feather: f32,
    bound_min: f32,
    bound_max: f32,
    invert: u32,
) -> f32 {
    let d = e_in.position - vec3<f32>(origin_x, origin_y, origin_z);

    var m: f32;
    if axis == 0u {
        m = d.x;
    } else if axis == 1u {
        m = d.y;
    } else if axis == 2u {
        m = d.z;
    } else if axis == 3u {
        m = length(vec2<f32>(d.x, d.z));
    } else {
        m = length(d);
    }

    let denom = max(bound_max - bound_min, 1e-6);
    let t = clamp((m - bound_min) / denom, 0.0, 1.0);
    let edge1 = phase + max(feather, 1e-6);
    var w = 1.0 - smoothstep(phase, edge1, t);
    w = select(w, 1.0 - w, invert == 1u);
    return w;
}
