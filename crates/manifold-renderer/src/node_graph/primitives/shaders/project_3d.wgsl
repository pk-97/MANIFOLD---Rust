// node.project_3d — project Array<MeshVertex> (3D positions) to
// Array<LinePoint> (2D screen coords) via either orthographic
// (matches WireframeZoo's XY-scale path) or perspective projection.
//
// Output range [0, 1] — recentred at (0.5, 0.5) and clamped via
// project_dist for perspective. Orthographic: out.xy = pos.xy *
// proj_scale + 0.5.

struct Project3DUniforms {
    active_count: u32,
    capacity: u32,
    mode: u32,         // 0=Orthographic, 1=Perspective
    _pad0: u32,
    proj_scale: f32,
    proj_dist: f32,
    _pad1: f32,
    _pad2: f32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
};

struct LinePoint {
    xy: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Project3DUniforms;
@group(0) @binding(1) var<storage, read> verts: array<MeshVertex>;
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
    var x: f32;
    var y: f32;

    if u.mode == 1u {
        // Perspective: s = proj_dist / (proj_dist + z)
        let dz = u.proj_dist + p.z;
        let s = u.proj_dist / max(dz, 0.001);
        x = p.x * s * u.proj_scale;
        y = p.y * s * u.proj_scale;
    } else {
        // Orthographic (matches WireframeZoo)
        x = p.x * u.proj_scale;
        y = p.y * u.proj_scale;
    }

    points[i].xy = vec2<f32>(0.5 + x, 0.5 + y);
}
