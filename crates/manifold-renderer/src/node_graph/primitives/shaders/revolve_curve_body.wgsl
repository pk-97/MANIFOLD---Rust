// node.revolve_curve — fusable BUFFER body (freeze §12, buffer domain), GATHER.
// Revolve a P-point profile curve (Array<CurvePoint>, x=radius, y=height)
// around the Y axis into a P×(segments+1) positions+uv grid (MESH_DEFORM_AND_
// CURVE_GEOMETRY_DESIGN.md D5 — normals left zero; wire node.make_triangles
// downstream for finite-difference normals + topology). Matches
// revolve_curve.wgsl bit-for-bit.
//
// ABI (buffer standalone codegen): `profile` is a BUFFER GATHER input — the
// output vertex reads one profile-row cell (clamped), so the body indexes the
// input array global `buf_profile` directly. `profile_len` is the derived
// element count of the profile buffer (Element = CurvePoint {x, y}).
//
// Row i (0..profile_len) reads the i-th profile point; column j
// (0..=segments) sweeps phi = sweep * j/segments. `sweep` is UNBOUNDED (range
// None at the param level, BUG-039 class) — full multi-turn sweeps are a
// valid performer gesture; cos/sin absorb the wrap with no seam. The seam
// column (j=0 vs j=segments) shares POSITION when sweep is an exact multiple
// of 2*pi but NOT uv (j/segments spans 0..1) — the deliberate D5
// seam-duplication contract that keeps make_triangles' finite-difference
// normals continuous across the seam.
fn rc_profile(row: i32, profile_len: i32) -> Element {
    let r = clamp(row, 0, profile_len - 1);
    return buf_profile[u32(r)];
}

fn body(idx: u32, count: u32, segments: i32, sweep: f32, profile_len: u32) -> Element2 {
    let cols = segments + 1;
    let p_len = i32(profile_len);
    let total = u32(p_len * cols);
    if idx >= total {
        // Padding vertex past the exact grid size (defensive — matches
        // make_triangles' padding contract for a dst buffer larger than the
        // exact computed count).
        return Element2(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 0.0), vec2<f32>(0.0, 0.0));
    }

    let row = i32(idx) / cols;
    let col = i32(idx) % cols;

    let pt = rc_profile(row, p_len);
    let radius = pt.x;
    let height = pt.y;

    let seg_f = max(f32(segments), 1.0);
    let phi = sweep * f32(col) / seg_f;
    let pos = vec3<f32>(radius * cos(phi), height, radius * sin(phi));

    let row_denom = max(f32(p_len - 1), 1.0);
    let uv = vec2<f32>(f32(col) / seg_f, f32(row) / row_denom);

    return Element2(pos, vec3<f32>(0.0, 0.0, 0.0), uv);
}
