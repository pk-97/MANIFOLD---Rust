// node.extrude_curve — HAND parity oracle for extrude_curve_body.wgsl.
// Extrudes an outline curve along +Z into a (steps+1) × cols grid (cols =
// outline_len, +1 when close duplicates the first column). Uniform
// layout/bindings match the generated standalone kernel (depth/steps/close
// params, then the derived outline_len, dispatch_count, pad) so the
// gpu_tests parity oracle packs ONE uniform for both kernels.

struct Uniforms {
    depth: f32,
    steps: i32,
    close: u32,
    outline_len: u32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
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
@group(0) @binding(1) var<storage, read> outline: array<CurvePoint>;
@group(0) @binding(2) var<storage, read_write> dst: array<MeshVertex>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.dispatch_count { return; }

    let p_len = max(i32(u.outline_len), 1);
    let is_closed = u.close != 0u;
    var cols = p_len;
    if is_closed {
        cols = p_len + 1;
    }
    let rows = u.steps + 1;
    let total = u32(cols * rows);
    if idx >= total {
        dst[idx].position = vec3<f32>(0.0, 0.0, 0.0);
        dst[idx]._pad0 = 0.0;
        dst[idx].normal = vec3<f32>(0.0, 0.0, 0.0);
        dst[idx]._pad1 = 0.0;
        dst[idx].uv = vec2<f32>(0.0, 0.0);
        dst[idx]._pad2 = vec2<f32>(0.0, 0.0);
        return;
    }

    let col = i32(idx) % cols;
    let row = i32(idx) / cols;
    let outline_col = col % p_len;
    let pt = outline[u32(outline_col)];

    let row_denom = max(f32(u.steps), 1.0);
    let col_denom = max(f32(cols - 1), 1.0);

    let pos = vec3<f32>(pt.x, pt.y, u.depth * f32(row) / row_denom);
    let uv = vec2<f32>(f32(col) / col_denom, f32(row) / row_denom);

    dst[idx].position = pos;
    dst[idx]._pad0 = 0.0;
    dst[idx].normal = vec3<f32>(0.0, 0.0, 0.0);
    dst[idx]._pad1 = 0.0;
    dst[idx].uv = uv;
    dst[idx]._pad2 = vec2<f32>(0.0, 0.0);
}
