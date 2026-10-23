// node.tube_from_path — fusable BUFFER body (freeze section 12, buffer domain),
// GATHER × 3. Sweep a circular ring around a K-point centerline path
// (Array<CurvePoint>, XZ plane — x=world X, y=world Z) into a positions+
// normals+uv tube grid, (sides+1) wide. Optional `lift` (+Y per path point)
// and `radius_scale` (per path point, composes with a ramp for tapered vines)
// both degrade to their identity value (0.0 / 1.0) past a short or unwired
// buffer — the same D2 degrade-to-default contract deformer `weights` inputs
// use. The CPU mirror is manifold_node_engine::scene::curve_sweep::tube_grid.
//
// Rows: one ring per path point, plus with `caps` a start cap (centre, rim)
// before them and an end cap (rim, centre) after, closing the path's ends.
// The rim row a cap shares with the side is doubled so the edge is hard.
//
// Frame per path point: tangent from a central finite difference (clamped at
// the path ends), reference-up = +Y (Gram-Schmidt orthogonalize to get
// `right`, then `ring_up = cross(tangent, right)`). **Documented limit
// (Deferred #4): this degenerates when the tangent is (near-)parallel to
// +Y — a vertical path segment.** The epsilon guard below only prevents a
// NaN/zero-length `right` vector; it does not fix the degeneracy —
// parallel-transport frames are deferred to a future 3D-path design.
//
// Normals are exact and face out of the tube (the ring's direction; a cap
// faces away from the path), oriented as make_triangles' finite differences
// orient them, so a push along the normals swells the tube. Where the tube
// is thicker than its path's tightest turn the inner wall folds through
// itself; finite differences flip there, the exact normals do not. Wire
// node.make_triangles with Keep Normals downstream.
fn tp_lift(k: u32, lift_len: u32) -> f32 {
    if k < lift_len {
        return buf_lift[k];
    }
    return 0.0;
}

fn tp_radius_scale(k: u32, radius_scale_len: u32) -> f32 {
    if k < radius_scale_len {
        return buf_radius_scale[k];
    }
    return 1.0;
}

fn tp_center(k: i32, lift_len: u32) -> vec3<f32> {
    let pt = buf_path[u32(k)];
    return vec3<f32>(pt.x, tp_lift(u32(k), lift_len), pt.y);
}

fn body(
    idx: u32,
    count: u32,
    radius: f32,
    sides: i32,
    caps: u32,
    path_len: u32,
    lift_len: u32,
    radius_scale_len: u32,
) -> Element2 {
    let p_len = max(i32(path_len), 1);
    let cols = sides + 1;
    var rows = p_len;
    if caps != 0u {
        rows = p_len + 4;
    }
    let total = u32(rows * cols);
    if idx >= total {
        return Element2(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 0.0), vec2<f32>(0.0, 0.0), vec2<f32>(0.0, 0.0), vec4<f32>(0.0), vec4<f32>(1.0));
    }

    let row = i32(idx) / cols;
    let col = i32(idx) % cols;
    // The path point, and the cap role: 0 side, 1 start, 2 end.
    var k = row;
    var cap = 0;
    var centre = false;
    if caps != 0u {
        if row <= 1 {
            k = 0;
            cap = 1;
            centre = row == 0;
        } else if row <= p_len + 1 {
            k = row - 2;
        } else {
            k = p_len - 1;
            cap = 2;
            centre = row == p_len + 3;
        }
    }

    let center = tp_center(k, lift_len);
    let rscale = tp_radius_scale(u32(k), radius_scale_len);
    let prev_c = tp_center(clamp(k - 1, 0, p_len - 1), lift_len);
    let next_c = tp_center(clamp(k + 1, 0, p_len - 1), lift_len);

    var tangent = next_c - prev_c;
    let t_len = length(tangent);
    if t_len < 1e-8 {
        tangent = vec3<f32>(0.0, 0.0, 1.0);
    } else {
        tangent = tangent / t_len;
    }

    let world_up = vec3<f32>(0.0, 1.0, 0.0);
    var right = cross(world_up, tangent);
    let r_len = length(right);
    if r_len < 1e-6 {
        right = vec3<f32>(1.0, 0.0, 0.0);
    } else {
        right = right / r_len;
    }
    let ring_up = cross(tangent, right);

    let sides_f = max(f32(sides), 1.0);
    // The ring turns clockwise seen down the path, so the grid's winding
    // faces out of the tube as its normals do. The last column wraps to
    // angle 0, so the seam shares exact positions and welds.
    let theta = -6.2831855 * f32(col % sides) / sides_f;
    let r_eff = radius * rscale;
    let ring = cos(theta) * right + sin(theta) * ring_up;
    var pos = center + ring * r_eff;
    var normal = ring;
    if r_eff < 0.0 {
        normal = -ring;
    }
    if cap == 1 {
        normal = -tangent;
    } else if cap == 2 {
        normal = tangent;
    }
    if centre {
        pos = center;
    }

    let row_denom = max(f32(p_len - 1), 1.0);
    let uv = vec2<f32>(f32(col) / sides_f, f32(k) / row_denom);

    return Element2(pos, normal, uv, vec2<f32>(0.0, 0.0), vec4<f32>(0.0), vec4<f32>(1.0));
}
