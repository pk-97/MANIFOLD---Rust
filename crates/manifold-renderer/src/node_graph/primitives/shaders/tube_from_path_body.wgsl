// node.tube_from_path — fusable BUFFER body (freeze §12, buffer domain),
// GATHER × 3. Sweep a circular ring around a K-point centerline path
// (Array<CurvePoint>, XZ plane — x=world X, y=world Z) into a K×(sides+1)
// positions+uv tube grid (MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md D5/D6 —
// normals left zero; wire node.make_triangles downstream). Optional `lift`
// (+Y per path point) and `radius_scale` (per path point, composes with a
// ramp for tapered vines) both degrade to their identity value (0.0 / 1.0)
// past a short or unwired buffer — the same D2 degrade-to-default contract
// deformer `weights` inputs use. Matches tube_from_path.wgsl bit-for-bit.
//
// Frame per path point: tangent from a central finite difference (clamped at
// the path ends), reference-up = +Y (Gram-Schmidt orthogonalize to get
// `right`, then `ring_up = cross(tangent, right)`). **Documented limit
// (Deferred #4): this degenerates when the tangent is (near-)parallel to
// +Y — a vertical path segment.** The epsilon guard below only prevents a
// NaN/zero-length `right` vector; it does not fix the degeneracy —
// parallel-transport frames are deferred to a future 3D-path design.
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

fn body(
    idx: u32,
    count: u32,
    radius: f32,
    sides: i32,
    path_len: u32,
    lift_len: u32,
    radius_scale_len: u32,
) -> Element2 {
    let p_len = max(i32(path_len), 1);
    let cols = sides + 1;
    let total = u32(p_len * cols);
    if idx >= total {
        return Element2(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 0.0), vec2<f32>(0.0, 0.0));
    }

    let row = i32(idx) / cols;
    let col = i32(idx) % cols;
    let k = u32(row);

    let pt = buf_path[k];
    let lift_v = tp_lift(k, lift_len);
    let rscale = tp_radius_scale(k, radius_scale_len);
    let center = vec3<f32>(pt.x, lift_v, pt.y);

    let k_prev = u32(clamp(row - 1, 0, p_len - 1));
    let k_next = u32(clamp(row + 1, 0, p_len - 1));
    let prev = buf_path[k_prev];
    let next = buf_path[k_next];
    let prev_c = vec3<f32>(prev.x, tp_lift(k_prev, lift_len), prev.y);
    let next_c = vec3<f32>(next.x, tp_lift(k_next, lift_len), next.y);

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
    let theta = 6.2831855 * f32(col) / sides_f;
    let r_eff = radius * rscale;
    let offset = (cos(theta) * right + sin(theta) * ring_up) * r_eff;
    let pos = center + offset;

    let row_denom = max(f32(p_len - 1), 1.0);
    let uv = vec2<f32>(f32(col) / sides_f, f32(row) / row_denom);

    return Element2(pos, vec3<f32>(0.0, 0.0, 0.0), uv);
}
