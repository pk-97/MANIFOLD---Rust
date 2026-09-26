// Signed boundary integration for closed, consistently wound volumes.
// Sum(clipped exit distance - clipped entry distance) excludes air gaps and
// truncates the medium at opaque geometry. This is a camera-ray approximation.
struct Vertex {
    position: vec3<f32>, _pad0: f32,
    normal: vec3<f32>, _pad1: f32,
    uv: vec2<f32>, _pad2: vec2<f32>, tangent: vec4<f32>,
};
struct Instance { pos_scale: vec4<f32>, rot_pad: vec4<f32> };
struct Uniforms {
    view_proj: mat4x4<f32>, model: mat4x4<f32>, inverse_view_proj: mat4x4<f32>,
    eye: vec4<f32>, parameters: vec4<f32>, appearance: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> vertices: array<Vertex>;
@group(0) @binding(2) var<storage, read> instances: array<Instance>;
@group(0) @binding(3) var opaque_depth: texture_depth_2d;
@group(0) @binding(4) var<storage, read> weights: array<f32>;

fn euler_xyz(a: vec3<f32>) -> mat3x3<f32> {
    let c = cos(a); let s = sin(a);
    let rx = mat3x3<f32>(vec3<f32>(1,0,0),vec3<f32>(0,c.x,s.x),vec3<f32>(0,-s.x,c.x));
    let ry = mat3x3<f32>(vec3<f32>(c.y,0,-s.y),vec3<f32>(0,1,0),vec3<f32>(s.y,0,c.y));
    let rz = mat3x3<f32>(vec3<f32>(c.z,s.z,0),vec3<f32>(-s.z,c.z,0),vec3<f32>(0,0,1));
    return rz * ry * rx;
}
struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) weight: f32,
    @location(2) @interpolate(flat) orientation: f32,
};
@vertex fn vs_main(@builtin(vertex_index) vid: u32, @builtin(instance_index) iid: u32) -> Out {
    let inst = instances[iid];
    let local = euler_xyz(inst.rot_pad.xyz) * (vertices[vid].position * inst.pos_scale.w) + inst.pos_scale.xyz;
    let world = u.model * vec4<f32>(local, 1);
    var out: Out;
    out.position = u.view_proj * world;
    out.world = world.xyz;
    out.weight = 1.0;
    if u.appearance.y > 0.5 { out.weight = weights[vid]; }
    out.orientation = sign(determinant(mat3x3<f32>(u.model[0].xyz, u.model[1].xyz, u.model[2].xyz)) * inst.pos_scale.w);
    return out;
}
@fragment fn fs_path(in: Out, @builtin(front_facing) front: bool) -> @location(0) f32 {
    if u.appearance.x * in.weight <= 0.0 { discard; }
    let pixel = vec2<i32>(in.position.xy);
    let depth = textureLoad(opaque_depth, pixel, 0);
    let dims = vec2<f32>(textureDimensions(opaque_depth));
    let uv = in.position.xy / dims;
    let opaque_h = u.inverse_view_proj * vec4<f32>(uv * vec2<f32>(2,-2) + vec2<f32>(-1,1), depth, 1);
    let near_h = u.inverse_view_proj * vec4<f32>(uv * vec2<f32>(2,-2) + vec2<f32>(-1,1), 1.0, 1.0);
    let origin = near_h.xyz / near_h.w;
    let opaque_distance = length(opaque_h.xyz / opaque_h.w - origin);
    let distance = min(length(in.world - origin), opaque_distance);
    // manifold-gpu raster winding: the camera-facing boundary is !front.
    return select(-distance, distance, front) * in.orientation * u.parameters.x;
}
@fragment fn fs_nearest(in: Out) {
    if u.appearance.x * in.weight <= 0.0 { discard; }
}
