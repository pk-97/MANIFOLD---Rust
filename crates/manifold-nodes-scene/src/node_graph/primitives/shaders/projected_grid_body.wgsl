// node.projected_grid — fusable BUFFER body, SOURCE (no array inputs).
// A screen-space grid cast onto the water plane y = level (Johanson 2004;
// docs/OCEAN_SURFACE_DESIGN.md section 3.3). Row 0 is the horizon (y_top),
// the last row the bottom of the frame, so node.make_triangles' normals point
// up. A ray that misses the plane, or meets it past max_distance, is clamped
// to max_distance along its horizontal direction.
//
// ABI (buffer standalone codegen, source shape): (idx, count, params in
// PARAMS order, then the derived camera uniforms) → MeshVertex written to
// buf_vertices[idx]. `width`/`height` only feed `tan_x` CPU-side.

fn body(
    idx: u32,
    count: u32,
    columns: i32,
    rows: i32,
    level: f32,
    max_distance: f32,
    margin: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
    width: f32,
    height: f32,
    cam_px: f32,
    cam_py: f32,
    cam_pz: f32,
    fwd_x: f32,
    fwd_y: f32,
    fwd_z: f32,
    right_x: f32,
    right_y: f32,
    right_z: f32,
    up_x: f32,
    up_y: f32,
    up_z: f32,
    tan_x: f32,
    tan_y: f32,
    y_top: f32,
) -> Element {
    let cols = u32(max(columns, 2));
    let rws = u32(max(rows, 2));
    let color = vec4<f32>(color_r, color_g, color_b, 1.0);
    if idx >= cols * rws {
        return Element(vec3<f32>(0.0), vec3<f32>(0.0, 1.0, 0.0), vec2<f32>(0.0), vec2<f32>(0.0), vec4<f32>(0.0), color);
    }
    let col = idx % cols;
    let row = idx / cols;
    let u = f32(col) / f32(cols - 1u);
    let v = f32(row) / f32(rws - 1u);
    let edge = 1.0 + margin;
    let sx = (2.0 * u - 1.0) * edge;
    let sy = mix(y_top, -edge, v);
    let cam = vec3<f32>(cam_px, cam_py, cam_pz);
    let r = vec3<f32>(fwd_x, fwd_y, fwd_z)
        + sx * tan_x * vec3<f32>(right_x, right_y, right_z)
        + sy * tan_y * vec3<f32>(up_x, up_y, up_z);
    let above = cam.y - level;
    let horiz = length(r.xz);
    var dir = vec2<f32>(fwd_x, fwd_z);
    if horiz > 1e-6 {
        dir = r.xz / horiz;
    } else if length(dir) < 1e-6 {
        dir = vec2<f32>(1.0, 0.0);
    } else {
        dir = normalize(dir);
    }
    var hit = cam.xz + dir * max_distance;
    if r.y < 0.0 && above > 0.0 {
        let t = above / -r.y;
        if t * horiz <= max_distance {
            hit = cam.xz + r.xz * t;
        }
    }
    let p = vec3<f32>(hit.x, level, hit.y);
    return Element(p, vec3<f32>(0.0, 1.0, 0.0), hit, vec2<f32>(0.0), vec4<f32>(0.0), color);
}
