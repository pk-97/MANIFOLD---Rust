// node.render_scene: last frame's vertex positions for a mesh that deforms.
// One thread per vertex record copies the record's position into the array
// the velocity pass reads next frame, so each vertex reports its own motion
// instead of only its object's. `w` is 1 so the record is a point.
//
// MeshVertex is 80 bytes with the position first (manifold-node-engine
// mesh.rs); the layout below mirrors render_scene.wgsl's `Vertex`.

struct PackArgs {
    // Records to copy: the vertex records the draw can read.
    count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

struct Vertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
    tangent: vec4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: PackArgs;
@group(0) @binding(1) var<storage, read> verts: array<Vertex>;
@group(0) @binding(2) var<storage, read_write> prev: array<vec4<f32>>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.count {
        return;
    }
    prev[gid.x] = vec4<f32>(verts[gid.x].position, 1.0);
}
