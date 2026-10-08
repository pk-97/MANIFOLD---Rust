// Shared cut-map validation before converting a source index or gathering.
fn valid_cut_mapping(mapping: vec4<f32>) -> bool {
    return all(mapping == mapping)
        && all(abs(mapping) <= vec4<f32>(3.402823466e+38))
        && mapping.w >= 0.0 && mapping.w <= 16777215.0
        && mapping.w == floor(mapping.w)
        && all(mapping.xyz >= vec3<f32>(-1e-6))
        && all(mapping.xyz <= vec3<f32>(1.000001))
        && abs(mapping.x + mapping.y + mapping.z - 1.0) <= 1e-5;
}
