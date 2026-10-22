// node.revolve_curve — fusable BUFFER body (freeze section 12, buffer domain), GATHER.
// Revolve a P-point profile curve (Array<CurvePoint>, x=radius, y=height)
// around the Y axis into a P×(segments+1) positions+normals+uv grid. The CPU
// mirror is manifold_node_engine::scene::curve_sweep::revolve_grid.
//
// ABI (buffer standalone codegen): `profile` is a BUFFER GATHER input — the
// body indexes the input array global `buf_profile` directly. `profile_len`
// is the derived element count of the profile buffer (Element = CurvePoint
// {x, y}).
//
// Row i (0..profile_len) reads the i-th profile point; column j
// (0..=segments) sweeps phi = sweep * j/segments. `sweep` is UNBOUNDED (range
// None at the param level, BUG-039 (unbounded sweep param)) — full multi-turn
// sweeps are a valid performer gesture; cos/sin absorb the wrap with no seam.
// The seam column (j=0 vs j=segments) shares POSITION when sweep is an exact
// multiple of 2*pi but NOT uv (j/segments spans 0..1).
//
// Normals are exact: the profile's normal turned to angle phi, oriented as
// make_triangles' finite differences orient them, and defined on the axis
// too (a profile point at radius 0). Wire node.make_triangles with Keep
// Normals downstream.
fn rc_profile(row: i32, profile_len: i32) -> vec2<f32> {
    let r = clamp(row, 0, profile_len - 1);
    let pt = buf_profile[u32(r)];
    return vec2<f32>(pt.x, pt.y);
}

// The profile's direction at row i: a central difference, wrapping round a
// profile whose end point repeats its start, one-sided at an open end.
fn rc_tangent(i: i32, p_len: i32) -> vec2<f32> {
    if p_len < 2 {
        return vec2<f32>(0.0, 0.0);
    }
    var prev = max(i - 1, 0);
    var next = min(i + 1, p_len - 1);
    let first = rc_profile(0, p_len);
    if p_len > 2 && length(rc_profile(p_len - 1, p_len) - first) <= 1e-5 * max(length(first), 1.0) {
        let distinct = p_len - 1;
        let j = i % distinct;
        prev = (j + distinct - 1) % distinct;
        next = (j + 1) % distinct;
    }
    return rc_profile(next, p_len) - rc_profile(prev, p_len);
}

fn rc_sign(v: f32) -> f32 {
    if v < 0.0 {
        return -1.0;
    }
    return 1.0;
}

fn body(idx: u32, count: u32, segments: i32, sweep: f32, profile_len: u32) -> Element2 {
    let cols = segments + 1;
    let p_len = i32(profile_len);
    let total = u32(p_len * cols);
    if idx >= total {
        // Padding vertex past the exact grid size (defensive — matches
        // make_triangles' padding contract for a dst buffer larger than the
        // exact computed count).
        return Element2(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 0.0), vec2<f32>(0.0, 0.0), vec2<f32>(0.0, 0.0), vec4<f32>(0.0), vec4<f32>(1.0));
    }

    let row = i32(idx) / cols;
    let col = i32(idx) % cols;

    let pt = rc_profile(row, p_len);
    let radius = pt.x;
    let height = pt.y;

    let seg_f = max(f32(segments), 1.0);
    let phi = sweep * f32(col) / seg_f;
    let c = cos(phi);
    let s = sin(phi);
    let pos = vec3<f32>(radius * c, height, radius * s);

    let t = rc_tangent(row, p_len);
    let n = vec3<f32>(t.y * c, -t.x, t.y * s) * (rc_sign(radius) * rc_sign(sweep));
    var normal = vec3<f32>(0.0, 1.0, 0.0);
    if length(n) >= 1e-8 {
        normal = normalize(n);
    }

    let row_denom = max(f32(p_len - 1), 1.0);
    let uv = vec2<f32>(f32(col) / seg_f, f32(row) / row_denom);

    return Element2(pos, normal, uv, vec2<f32>(0.0, 0.0), vec4<f32>(0.0), vec4<f32>(1.0));
}
