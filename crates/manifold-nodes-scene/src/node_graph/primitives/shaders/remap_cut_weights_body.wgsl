// `node.remap_cut_weights` — scalar gather through the shared cut map.
fn body(idx: u32, count: u32, e_map: Element) -> f32 {
    let mapping = e_map;
    if !valid_cut_mapping(vec4<f32>(mapping.x, mapping.y, mapping.z, mapping.w)) { return 0.0; }
    let triangle = u32(mapping.w);
    let input_len = arrayLength(&buf_in);
    if triangle >= input_len / 3u { return 0.0; }
    let base = triangle * 3u;
    if input_len - base < 3u { return 0.0; }
    let bary = vec3<f32>(mapping.x, mapping.y, mapping.z);
    return buf_in[base] * bary.x + buf_in[base + 1u] * bary.y + buf_in[base + 2u] * bary.z;
}
