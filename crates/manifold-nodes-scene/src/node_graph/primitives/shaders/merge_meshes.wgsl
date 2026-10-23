// node.merge_meshes — copy one part's vertices into the merged mesh at its
// offset, placed by the part's matrix. One dispatch per part; `clear`
// writes zeroed vertices instead (the tail past every part).

struct Params {
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    m3: vec4<f32>,
    // The inverse transpose of the matrix's linear part, for normals.
    n0: vec4<f32>,
    n1: vec4<f32>,
    n2: vec4<f32>,
    offset: u32,
    count: u32,
    clear: u32,
    _pad: u32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    uv1: vec2<f32>,
    tangent: vec4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> part: array<MeshVertex>;
@group(0) @binding(2) var<storage, read_write> merged: array<MeshVertex>;

fn unit_or_zero(v: vec3<f32>) -> vec3<f32> {
    let l = length(v);
    return select(vec3<f32>(0.0), v / l, l > 0.0);
}

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= params.count {
        return;
    }
    let at = params.offset + i;
    if at >= arrayLength(&merged) {
        return;
    }
    if params.clear != 0u {
        var empty: MeshVertex;
        merged[at] = empty;
        return;
    }
    var v = part[i];
    let m = mat4x4<f32>(params.m0, params.m1, params.m2, params.m3);
    let n = mat3x3<f32>(params.n0.xyz, params.n1.xyz, params.n2.xyz);
    let linear = mat3x3<f32>(params.m0.xyz, params.m1.xyz, params.m2.xyz);
    v.position = (m * vec4<f32>(v.position, 1.0)).xyz;
    v.normal = unit_or_zero(n * v.normal);
    v.tangent = vec4<f32>(unit_or_zero(linear * v.tangent.xyz), v.tangent.w);
    merged[at] = v;
}
