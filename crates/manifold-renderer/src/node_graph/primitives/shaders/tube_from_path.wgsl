// node.tube_from_path — HAND parity oracle for tube_from_path_body.wgsl.
// Sweeps a ring around a centerline path (XZ plane) into a
// path_len × (sides+1) tube grid. Uniform layout/bindings match the
// generated standalone kernel (radius/sides params, then the derived
// path_len/lift_len/radius_scale_len, dispatch_count, pad) so the gpu_tests
// parity oracle packs ONE uniform for both kernels.

struct Uniforms {
    radius: f32,
    sides: i32,
    path_len: u32,
    lift_len: u32,
    radius_scale_len: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
};

struct CurvePoint {
    x: f32,
    y: f32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> path: array<CurvePoint>;
@group(0) @binding(2) var<storage, read> lift: array<f32>;
@group(0) @binding(3) var<storage, read> radius_scale: array<f32>;
@group(0) @binding(4) var<storage, read_write> dst: array<MeshVertex>;

fn hand_lift(k: u32) -> f32 {
    if k < u.lift_len {
        return lift[k];
    }
    return 0.0;
}

fn hand_radius_scale(k: u32) -> f32 {
    if k < u.radius_scale_len {
        return radius_scale[k];
    }
    return 1.0;
}

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.dispatch_count { return; }

    let p_len = max(i32(u.path_len), 1);
    let cols = u.sides + 1;
    let total = u32(p_len * cols);
    if idx >= total {
        dst[idx].position = vec3<f32>(0.0, 0.0, 0.0);
        dst[idx]._pad0 = 0.0;
        dst[idx].normal = vec3<f32>(0.0, 0.0, 0.0);
        dst[idx]._pad1 = 0.0;
        dst[idx].uv = vec2<f32>(0.0, 0.0);
        dst[idx]._pad2 = vec2<f32>(0.0, 0.0);
        return;
    }

    let row = i32(idx) / cols;
    let col = i32(idx) % cols;
    let k = u32(row);

    let pt = path[k];
    let lift_v = hand_lift(k);
    let rscale = hand_radius_scale(k);
    let center = vec3<f32>(pt.x, lift_v, pt.y);

    let k_prev = u32(clamp(row - 1, 0, p_len - 1));
    let k_next = u32(clamp(row + 1, 0, p_len - 1));
    let prev = path[k_prev];
    let next = path[k_next];
    let prev_c = vec3<f32>(prev.x, hand_lift(k_prev), prev.y);
    let next_c = vec3<f32>(next.x, hand_lift(k_next), next.y);

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

    let sides_f = max(f32(u.sides), 1.0);
    let theta = 6.2831855 * f32(col) / sides_f;
    let r_eff = u.radius * rscale;
    let offset = (cos(theta) * right + sin(theta) * ring_up) * r_eff;
    let pos = center + offset;

    let row_denom = max(f32(p_len - 1), 1.0);
    let uv = vec2<f32>(f32(col) / sides_f, f32(row) / row_denom);

    dst[idx].position = pos;
    dst[idx]._pad0 = 0.0;
    dst[idx].normal = vec3<f32>(0.0, 0.0, 0.0);
    dst[idx]._pad1 = 0.0;
    dst[idx].uv = uv;
    dst[idx]._pad2 = vec2<f32>(0.0, 0.0);
}
