// Production FLIP field contract: native support and solid clipping,
// with no preview-only border override. Test references are independent gathers.
// Names avoid particle_volume_common.wgsl, which the codegen prepends.

fn pv_ref_solid(p: vec3<f32>, lattice_min: vec3<f32>, spacing: vec3<f32>, nodes: vec3<u32>) -> f32 {
    let g = clamp((p - lattice_min) / spacing, vec3<f32>(0.0), vec3<f32>(nodes - vec3<u32>(1u)));
    let base = min(vec3<u32>(floor(g)), nodes - vec3<u32>(2u));
    let f = g - vec3<f32>(base);
    var value = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, (corner >> 2u) & 1u);
        let at = base + o;
        let w = select(1.0 - f.x, f.x, o.x == 1u)
            * select(1.0 - f.y, f.y, o.y == 1u)
            * select(1.0 - f.z, f.z, o.z == 1u);
        value = value + w * buf_solid[at.x + nodes.x * (at.y + nodes.y * at.z)];
    }
    return value;
}

fn body(
    idx: u32,
    count: u32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    resolution_scale: i32,
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
    band_extra: f32,
) -> f32 {
    var radius = 0.0;
    for (var k = 0u; k < arrayLength(&buf_blobs); k += 1u) { radius = max(radius, buf_blobs[k].center_radius.w); }
    let band = 3.0 * radius;
    let solid_nodes = max(vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z)), vec3<u32>(2u));
    let scale = u32(clamp(resolution_scale, 1, 8));
    let nodes = (solid_nodes - vec3<u32>(1u)) * scale + vec3<u32>(1u);
    let bins = vec3<i32>(bins_x, bins_y, bins_z);
    if idx >= nodes.x * nodes.y * nodes.z || any(bins < vec3<i32>(1)) {
        return band;
    }
    let ijk = vec3<u32>(idx % nodes.x, (idx / nodes.x) % nodes.y, idx / (nodes.x * nodes.y));
    let size = vec3<f32>(size_x, size_y, size_z);
    let lattice_min = vec3<f32>(center_x, center_y, center_z) - 0.5 * size;
    let p = lattice_min + vec3<f32>(ijk) * size / vec3<f32>(nodes - vec3<u32>(1u));

    var phi = band;
    let h = size / vec3<f32>(nodes - vec3<u32>(1u));
    for (var k = 0u; k < arrayLength(&buf_blobs); k += 1u) {
        let blob = buf_blobs[k];
        let r = blob.center_radius.w;
        if !(r > 0.0) { continue; }
        let first = vec3<i32>(floor((blob.center_radius.xyz - vec3<f32>(1.5 * r) - lattice_min) / h));
        let last = vec3<i32>(floor((blob.center_radius.xyz + vec3<f32>(1.5 * r) - lattice_min) / h)) + vec3<i32>(1);
        if any(vec3<i32>(ijk) < first) || any(vec3<i32>(ijk) > last) { continue; }
        let d = p - blob.center_radius.xyz;
        let diag = blob.shape_diag;
        let off = blob.shape_off;
        let v = vec3<f32>(diag.x*d.x + off.x*d.y + off.y*d.z,
            off.x*d.x + diag.y*d.y + off.z*d.z,
            off.y*d.x + off.z*d.y + diag.z*d.z);
        phi = min(phi, r * (length(v) - 1.0));
    }
    let spacing = size / vec3<f32>(solid_nodes - vec3<u32>(1u));
    if pv_ref_solid(p, lattice_min, spacing, solid_nodes) < 0.0 {
        phi = max(phi, 0.0);
    }
    return phi;
}
