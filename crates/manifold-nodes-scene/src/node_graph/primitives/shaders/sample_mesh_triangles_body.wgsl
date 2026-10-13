fn body(idx: u32, count: u32, density: f32) -> Element {
    let d = u32(clamp(round(density), 2.0, 8.0));
    // buf_live_count[0] is the live vertex count at the front of buf_in;
    // u32::MAX when unwired.
    let total = min(arrayLength(&buf_in), buf_live_count[0]) / 3u;
    let sample_count = min(d*d*d, total);
    if idx / 3u >= sample_count {
        return Element(vec3<f32>(0.0),vec3<f32>(0.0),vec2<f32>(0.0),vec2<f32>(0.0),vec4<f32>(0.0), vec4<f32>(1.0));
    }
    let v = buf_in[source_face_index(idx / 3u,total,sample_count)*3u + idx%3u];
    return Element(v.position,v.normal,v.uv,v.uv1,v.tangent,v.color);
}
