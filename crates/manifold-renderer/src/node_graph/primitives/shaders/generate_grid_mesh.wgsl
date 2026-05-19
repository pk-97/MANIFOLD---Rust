// node.generate_grid_mesh — emit an Array<MeshVertex> sized
// resolution_x * resolution_y, laid out as a flat plane in the
// XZ plane (Y=0 by default). One thread per vertex.
//
// Phase B of BUFFER_PORT_PLAN. MetallicGlass's 500×500
// displacement grid materialises through this primitive instead
// of being implicit in a vertex-shader vertex_index hack. The
// downstream Render3DMesh consumes the buffer; intermediate
// primitives can displace, normal-recompute, or color it.
//
// Layout matches generators::mesh_common::MeshVertex (32 bytes):
//   position: vec3<f32> + pad
//   normal:   vec3<f32> + pad

struct GridUniforms {
    resolution_x: u32,
    resolution_y: u32,
    capacity: u32,
    _pad0: u32,
    size_x: f32,
    size_y: f32,
    origin_x: f32,
    origin_z: f32,
};

struct Vertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> params: GridUniforms;
@group(0) @binding(1) var<storage, read_write> vertices: array<Vertex>;

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.capacity {
        return;
    }
    if i >= params.resolution_x * params.resolution_y {
        // Inactive slot — clear to a dead/zero vertex.
        vertices[i].position = vec3<f32>(0.0, 0.0, 0.0);
        vertices[i]._pad0 = 0.0;
        vertices[i].normal = vec3<f32>(0.0, 1.0, 0.0);
        vertices[i]._pad1 = 0.0;
        return;
    }

    let row = i / params.resolution_x;
    let col = i % params.resolution_x;
    let nx = f32(col) / f32(max(params.resolution_x - 1u, 1u));
    let nz = f32(row) / f32(max(params.resolution_y - 1u, 1u));

    let x = params.origin_x + (nx - 0.5) * params.size_x;
    let z = params.origin_z + (nz - 0.5) * params.size_y;

    vertices[i].position = vec3<f32>(x, 0.0, z);
    vertices[i]._pad0 = 0.0;
    vertices[i].normal = vec3<f32>(0.0, 1.0, 0.0);
    vertices[i]._pad1 = 0.0;
}
