// `node.magnitude_db` — linear magnitude to decibels.
// PARAMS: [floor_db, reference]
fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, floor_db: f32, reference: f32) -> vec4<f32> {
    let safe_reference = max(reference, 1.0e-6);
    let db = max(20.0 * log2(max(c.rgb, vec3<f32>(1.0e-6)) / safe_reference) * 0.30102999566, vec3<f32>(floor_db));
    return vec4<f32>(db, c.a);
}
