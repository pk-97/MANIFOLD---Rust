// node.triangulate_grid — convert a positions-only NxM grid of
// MeshVertex into a triangle-list of (N-1)*(M-1)*6 vertices ready
// for node.render_3d_mesh. Computes per-vertex normals from
// finite-difference tangents in the source grid.
//
// One workgroup thread per output triangle vertex (six vertices per
// quad, one quad per (col, row) in [0, N-1)×[0, M-1)).

struct TriangulateUniforms {
    src_cols: u32,
    src_rows: u32,
    dst_capacity: u32,
    _pad0: u32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: TriangulateUniforms;
@group(0) @binding(1) var<storage, read> src: array<MeshVertex>;
@group(0) @binding(2) var<storage, read_write> dst: array<MeshVertex>;

fn sample_pos(col: i32, row: i32) -> vec3<f32> {
    let cc = clamp(col, 0, i32(u.src_cols) - 1);
    let rr = clamp(row, 0, i32(u.src_rows) - 1);
    let idx = u32(rr) * u.src_cols + u32(cc);
    return src[idx].position;
}

fn compute_normal(col: i32, row: i32) -> vec3<f32> {
    let center = sample_pos(col, row);
    let dx = sample_pos(col + 1, row) - sample_pos(col - 1, row);
    let dy = sample_pos(col, row + 1) - sample_pos(col, row - 1);
    let n = cross(dy, dx);
    let len = length(n);
    if len < 1e-8 {
        return vec3<f32>(0.0, 1.0, 0.0);
    }
    let _ = center;
    return n / len;
}

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= u.dst_capacity {
        return;
    }

    let quads_x = u.src_cols - 1u;
    let quads_y = u.src_rows - 1u;
    let total_verts = quads_x * quads_y * 6u;

    if i >= total_verts {
        // Pad the rest with zero verts so the output buffer is fully
        // initialised (Render3DMesh truncates to a multiple of 3, so
        // these are skipped anyway).
        dst[i].position = vec3<f32>(0.0, 0.0, 0.0);
        dst[i]._pad0 = 0.0;
        dst[i].normal = vec3<f32>(0.0, 1.0, 0.0);
        dst[i]._pad1 = 0.0;
        return;
    }

    let quad_idx = i / 6u;
    let corner = i % 6u;
    let qx = quad_idx % quads_x;
    let qy = quad_idx / quads_x;

    // Triangle layout (matches the procedural pattern in
    // metallic_glass_render.wgsl's vertex shader):
    //   0: (qx, qy)        4: (qx+1, qy)
    //   1: (qx+1, qy)      5: (qx+1, qy+1)
    //   2: (qx, qy+1)
    //   3: (qx, qy+1)
    var dx: u32 = 0u;
    var dy: u32 = 0u;
    switch corner {
        case 0u: { dx = 0u; dy = 0u; }
        case 1u: { dx = 1u; dy = 0u; }
        case 2u: { dx = 0u; dy = 1u; }
        case 3u: { dx = 0u; dy = 1u; }
        case 4u: { dx = 1u; dy = 0u; }
        case 5u: { dx = 1u; dy = 1u; }
        default: {}
    }

    let col = i32(qx + dx);
    let row = i32(qy + dy);
    let pos = sample_pos(col, row);
    let normal = compute_normal(col, row);

    dst[i].position = pos;
    dst[i]._pad0 = 0.0;
    dst[i].normal = normal;
    dst[i]._pad1 = 0.0;
}
