// node.rotate_4d — apply 4D rotation (XY, ZW, XW planes) to an
// Array<Vec4Vertex>. Phase B of BUFFER_PORT_PLAN.
//
// Mirrors generator_math::rotate_4d so the Tesseract / Duocylinder
// / WireframeZoo behaviour is bit-identical when the user wires
// the same base verts through this primitive instead of calling
// the CPU helper directly.
//
// Vec4Vertex layout (16 bytes): position: vec4<f32>

struct RotateUniforms {
    active_count: u32,
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
    angle_xy: f32,
    angle_zw: f32,
    angle_xw: f32,
    _pad2: f32,
};

struct Vec4Vertex {
    position: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: RotateUniforms;
@group(0) @binding(1) var<storage, read> input: array<Vec4Vertex>;
@group(0) @binding(2) var<storage, read_write> output: array<Vec4Vertex>;

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.capacity {
        return;
    }
    if i >= params.active_count {
        output[i].position = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        return;
    }

    var p = input[i].position;
    var x = p.x;
    var y = p.y;
    var z = p.z;
    var w = p.w;

    // XY plane
    let cxy = cos(params.angle_xy);
    let sxy = sin(params.angle_xy);
    let nx_xy = x * cxy - y * sxy;
    let ny_xy = x * sxy + y * cxy;
    x = nx_xy;
    y = ny_xy;

    // ZW plane
    let czw = cos(params.angle_zw);
    let szw = sin(params.angle_zw);
    let nz_zw = z * czw - w * szw;
    let nw_zw = z * szw + w * czw;
    z = nz_zw;
    w = nw_zw;

    // XW plane
    let cxw = cos(params.angle_xw);
    let sxw = sin(params.angle_xw);
    let nx_xw = x * cxw - w * sxw;
    let nw_xw = x * sxw + w * cxw;
    x = nx_xw;
    w = nw_xw;

    output[i].position = vec4<f32>(x, y, z, w);
}
