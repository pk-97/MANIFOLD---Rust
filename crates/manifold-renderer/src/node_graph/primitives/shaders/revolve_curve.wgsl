// node.revolve_curve — HAND parity oracle for revolve_curve_body.wgsl.
// Revolves a profile curve (x=radius, y=height) around Y into a
// profile_len × (segments+1) grid. Uniform layout/bindings match the
// generated standalone kernel (segments/sweep params, then the derived
// profile_len, dispatch_count) so the gpu_tests parity oracle packs ONE
// uniform for both kernels.

struct Uniforms {
    segments: i32,
    sweep: f32,
    profile_len: u32,
    dispatch_count: u32,
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
@group(0) @binding(1) var<storage, read> profile: array<CurvePoint>;
@group(0) @binding(2) var<storage, read_write> dst: array<MeshVertex>;

fn sample_profile(row: i32) -> CurvePoint {
    let r = clamp(row, 0, i32(u.profile_len) - 1);
    return profile[u32(r)];
}

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.dispatch_count { return; }

    let cols = u.segments + 1;
    let p_len = i32(u.profile_len);
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

    let pt = sample_profile(row);
    let seg_f = max(f32(u.segments), 1.0);
    let phi = u.sweep * f32(col) / seg_f;
    let pos = vec3<f32>(pt.x * cos(phi), pt.y, pt.x * sin(phi));

    let row_denom = max(f32(p_len - 1), 1.0);
    let uv = vec2<f32>(f32(col) / seg_f, f32(row) / row_denom);

    dst[idx].position = pos;
    dst[idx]._pad0 = 0.0;
    dst[idx].normal = vec3<f32>(0.0, 0.0, 0.0);
    dst[idx]._pad1 = 0.0;
    dst[idx].uv = uv;
    dst[idx]._pad2 = vec2<f32>(0.0, 0.0);
}
