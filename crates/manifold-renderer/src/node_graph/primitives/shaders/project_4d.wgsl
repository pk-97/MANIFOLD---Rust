// node.project_4d — project Array<Vec4Vertex> → Array<LinePoint>
// via two-stage perspective (4D → 3D → 2D). Matches
// generator_math::project_4d bit-for-bit.
//
// Stage 1: f = proj_dist / (proj_dist - w); p3 = xyz * f
// Stage 2: s = proj_dist / (proj_dist + p3z); px = p3x * s * PROJ_SCALE
// Output is in [0, 1] screen space, centered at (0.5, 0.5).

struct Project4DUniforms {
    active_count: u32,
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
    proj_scale: f32,
    proj_dist: f32,
    _pad2: f32,
    _pad3: f32,
};

struct Vec4Vertex {
    position: vec4<f32>,
};

struct LinePoint {
    xy: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Project4DUniforms;
@group(0) @binding(1) var<storage, read> verts: array<Vec4Vertex>;
@group(0) @binding(2) var<storage, read_write> points: array<LinePoint>;

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= u.capacity {
        return;
    }
    if i >= u.active_count {
        points[i].xy = vec2<f32>(0.5, 0.5);
        return;
    }

    let p = verts[i].position;
    let dw = u.proj_dist - p.w;
    let f = u.proj_dist / select(dw, 0.001, abs(dw) < 0.001);
    let p3 = vec3<f32>(p.x, p.y, p.z) * f;

    let dz = u.proj_dist + p3.z;
    let s = u.proj_dist / select(dz, 0.001, abs(dz) < 0.001);

    let px = p3.x * s * u.proj_scale;
    let py = p3.y * s * u.proj_scale;

    points[i].xy = vec2<f32>(0.5 + px, 0.5 + py);
}
