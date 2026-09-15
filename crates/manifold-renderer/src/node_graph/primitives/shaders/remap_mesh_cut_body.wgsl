// `node.remap_mesh_cut` — gather a MeshVertex through a triangle/barycentric
// map. The map's w component is an exact f32 source triangle index; negative w
// is reserved for deterministic padding and must not create visible arrows.
fn orthogonal_frame(normal_in: vec3<f32>, tangent_in: vec4<f32>) -> Element {
    let normal_length = length(normal_in);
    var normal = vec3<f32>(0.0, 1.0, 0.0);
    if normal_length > 1e-12 { normal = normal_in / normal_length; }
    let tangent_xyz = tangent_in.xyz - normal * dot(tangent_in.xyz, normal);
    let tangent_length = length(tangent_xyz);
    var tangent = vec3<f32>(0.0, 0.0, 0.0);
    if tangent_length > 1e-12 { tangent = tangent_xyz / tangent_length; }
    return Element(vec3<f32>(0.0), normal, vec2<f32>(0.0), vec4<f32>(tangent, tangent_in.w));
}

fn body(idx: u32, count: u32, e_map: Element2) -> Element {
    let mapping = e_map;
    if !valid_cut_mapping(vec4<f32>(mapping.x, mapping.y, mapping.z, mapping.w)) {
        return orthogonal_frame(vec3<f32>(0.0, 1.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0));
    }
    let triangle = u32(mapping.w);
    let input_len = arrayLength(&buf_in);
    if triangle >= input_len / 3u {
        return orthogonal_frame(vec3<f32>(0.0, 1.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0));
    }
    let base = triangle * 3u;
    if input_len - base < 3u {
        return orthogonal_frame(vec3<f32>(0.0, 1.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0));
    }
    let a = buf_in[base];
    let b = buf_in[base + 1u];
    let c = buf_in[base + 2u];
    let bary = vec3<f32>(mapping.x, mapping.y, mapping.z);
    if bary.x == 1.0 && bary.y == 0.0 && bary.z == 0.0 { return a; }
    if bary.x == 0.0 && bary.y == 1.0 && bary.z == 0.0 { return b; }
    if bary.x == 0.0 && bary.y == 0.0 && bary.z == 1.0 { return c; }
    let position = a.position * bary.x + b.position * bary.y + c.position * bary.z;
    let uv = a.uv * bary.x + b.uv * bary.y + c.uv * bary.z;
    let normal = a.normal * bary.x + b.normal * bary.y + c.normal * bary.z;
    let tangent = a.tangent * bary.x + b.tangent * bary.y + c.tangent * bary.z;
    let frame = orthogonal_frame(normal, tangent);
    return Element(position, frame.normal, uv, frame.tangent);
}
