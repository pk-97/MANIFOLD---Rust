// node.displace_mesh — perturb the Y component of an
// Array<MeshVertex> positions grid by sampling a height Texture2D.
// One thread per grid vertex.
//
// Vertex idx → (col, row): row-major, cols + rows from uniforms.
// UV → (col / (cols - 1), row / (rows - 1)).
// Y += (sample.r - 0.5) * displacement.

struct DisplaceUniforms {
    cols: u32,
    rows: u32,
    capacity: u32,
    _pad0: u32,
    displacement: f32,
    height_bias: f32,
    _pad1: f32,
    _pad2: f32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: DisplaceUniforms;
@group(0) @binding(1) var<storage, read> src: array<MeshVertex>;
@group(0) @binding(2) var<storage, read_write> dst: array<MeshVertex>;
@group(0) @binding(3) var height_tex: texture_2d<f32>;
@group(0) @binding(4) var height_sampler: sampler;

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= u.capacity {
        return;
    }

    let active_count = u.cols * u.rows;
    if i >= active_count {
        dst[i] = src[i];
        return;
    }

    let col = i % u.cols;
    let row = i / u.cols;
    let denom_c = f32(max(u.cols - 1u, 1u));
    let denom_r = f32(max(u.rows - 1u, 1u));
    let uv = vec2<f32>(f32(col) / denom_c, f32(row) / denom_r);

    let h_raw = textureSampleLevel(height_tex, height_sampler, uv, 0.0).r;
    let displaced_y = src[i].position.y + (h_raw - u.height_bias) * u.displacement;

    dst[i].position = vec3<f32>(src[i].position.x, displaced_y, src[i].position.z);
    dst[i]._pad0 = 0.0;
    dst[i].normal = src[i].normal;
    dst[i]._pad1 = 0.0;
}
