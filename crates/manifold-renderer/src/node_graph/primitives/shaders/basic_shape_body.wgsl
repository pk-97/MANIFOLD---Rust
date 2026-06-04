// node.basic_shape — fusable body (freeze §12), SOURCE. Renders one of three
// centred 2D SDF shapes (Square / Diamond / Octagon) with fwidth-antialiased
// edges. The body absorbs the run-side preprocessing the hand path used to do
// before packing its uniform: uv_scale = 1/scale (zoom out as scale grows),
// shape index straight from the enum, and the wireframe flag from a >0.5 test.
// So the generated Params carry the RAW params in declaration order (shape,
// aspect, scale, line, rotation, is_wireframe) rather than the hand uniform's
// preprocessed/reordered layout. Matches basic_shape.wgsl. PARAMS: [shape
// (Enum->u32), aspect, scale, line, rotation (Angle->f32), is_wireframe].
fn bs_rotate2d(p: vec2<f32>, angle: f32) -> vec2<f32> {
    let s = sin(angle);
    let c = cos(angle);
    return vec2<f32>(p.x * c - p.y * s, p.x * s + p.y * c);
}

fn bs_sd_square(p: vec2<f32>, size: f32) -> f32 {
    let d = abs(p) - vec2<f32>(size);
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0);
}

fn bs_sd_diamond(p: vec2<f32>, size: f32) -> f32 {
    let ap = abs(p);
    return (ap.x + ap.y - size) / 1.414213562;
}

fn bs_sd_octagon(p_in: vec2<f32>, r: f32) -> f32 {
    let k = vec3<f32>(-0.9238795325, 0.3826834323, 0.4142135623);
    var p = abs(p_in);
    p -= 2.0 * min(dot(vec2<f32>(k.x, k.y), p), 0.0) * vec2<f32>(k.x, k.y);
    p -= 2.0 * min(dot(vec2<f32>(-k.x, k.y), p), 0.0) * vec2<f32>(-k.x, k.y);
    p -= vec2<f32>(clamp(p.x, -k.z * r, k.z * r), r);
    return length(p) * sign(p.y);
}

fn bs_eval_sdf(p: vec2<f32>, shape: u32) -> f32 {
    switch shape {
        case 1u: { return bs_sd_diamond(p, 1.0); }
        case 2u: { return bs_sd_octagon(p, 1.0); }
        default: { return bs_sd_square(p, 1.0); }
    }
}

fn body(uv: vec2<f32>, dims: vec2<f32>, shape: u32, aspect: f32, scale: f32, line: f32, rotation: f32, is_wireframe: f32) -> vec4<f32> {
    let uv_scale = select(1.0, 1.0 / scale, scale > 0.0);

    var p_uv = uv - vec2<f32>(0.5);
    p_uv.x *= aspect;
    p_uv *= uv_scale;

    let wireframe = is_wireframe > 0.5;

    // Transform UV — the 0.315 screen-fit factor matches legacy BasicShapes.
    var p = p_uv / 0.315;
    p = bs_rotate2d(p, rotation);

    let d = bs_eval_sdf(p, shape);

    // Approximate fwidth(d) via finite differences one screen pixel away,
    // following the full transform chain (aspect → scale → /0.315 → rotate).
    let inv_scale = uv_scale / 0.315;
    let step_x = bs_rotate2d(vec2<f32>(inv_scale * aspect / dims.x, 0.0), rotation);
    let step_y = bs_rotate2d(vec2<f32>(0.0, inv_scale / dims.y), rotation);
    let d_dx = bs_eval_sdf(p + step_x, shape);
    let d_dy = bs_eval_sdf(p + step_y, shape);
    let fw = abs(d_dx - d) + abs(d_dy - d);

    let half_fw = fw * 0.5;
    var shape_v: f32;
    if wireframe {
        let wd = abs(d) - line;
        shape_v = 1.0 - smoothstep(-half_fw, half_fw, wd);
    } else {
        shape_v = 1.0 - smoothstep(-half_fw, half_fw, d);
    }

    let lum = saturate(shape_v);
    return vec4<f32>(lum, lum, lum, lum);
}
