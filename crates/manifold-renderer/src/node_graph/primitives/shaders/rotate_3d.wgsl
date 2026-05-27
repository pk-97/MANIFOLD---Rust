// node.rotate_3d — XYZ Euler rotation of an Array<MeshVertex>.
// Ports generator_math::rotate_3d bit-for-bit (X → Y → Z order).

struct Rotate3DUniforms {
    active_count: u32,
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
    angle_x: f32,
    angle_y: f32,
    angle_z: f32,
    _pad2: f32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
};

@group(0) @binding(0) var<uniform> params: Rotate3DUniforms;
@group(0) @binding(1) var<storage, read> input: array<MeshVertex>;
@group(0) @binding(2) var<storage, read_write> output: array<MeshVertex>;

fn rotate_xyz(p: vec3<f32>) -> vec3<f32> {
    let cx = cos(params.angle_x);
    let sx = sin(params.angle_x);
    let cy = cos(params.angle_y);
    let sy = sin(params.angle_y);
    let cz = cos(params.angle_z);
    let sz = sin(params.angle_z);

    var x = p.x;
    var y = p.y;
    var z = p.z;

    // Rotate around X (matches generator_math::rotate_3d ordering exactly)
    let ny1 = y * cx - z * sx;
    let nz1 = y * sx + z * cx;
    y = ny1;
    z = nz1;

    // Rotate around Y
    let nx2 = x * cy + z * sy;
    let nz2 = -x * sy + z * cy;
    x = nx2;
    z = nz2;

    // Rotate around Z
    let nx3 = x * cz - y * sz;
    let ny3 = x * sz + y * cz;
    x = nx3;
    y = ny3;

    return vec3<f32>(x, y, z);
}

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.capacity {
        return;
    }
    if i >= params.active_count {
        output[i] = input[i];
        return;
    }

    let pos = rotate_xyz(input[i].position);
    let normal = rotate_xyz(input[i].normal);

    output[i].position = pos;
    output[i]._pad0 = 0.0;
    output[i].normal = normal;
    output[i]._pad1 = 0.0;
    // UV is a parametric value on the surface — doesn't rotate with position.
    output[i].uv = input[i].uv;
    output[i]._pad2 = vec2<f32>(0.0, 0.0);
}
