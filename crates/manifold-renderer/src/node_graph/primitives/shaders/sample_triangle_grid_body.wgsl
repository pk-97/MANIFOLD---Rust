// Deterministic bounded sample lattice. Each cell owns one tiny triangle.
fn body(idx: u32, count: u32, density: i32, radius: f32, source_offset_x: f32, source_offset_y: f32, source_offset_z: f32) -> Element {
    let d = u32(clamp(density, 2, 8));
    let tri = idx / 3u;
    let corner = idx % 3u;
    let cell = tri;
    let x = cell % d; let y = (cell / d) % d; let z = cell / (d * d);
    if z >= d { return Element(vec3<f32>(0.0), vec3<f32>(0.0,1.0,0.0), vec2<f32>(0.0), vec4<f32>(0.0)); }
    let step = radius / f32(d); let base = vec3<f32>(f32(x), f32(y), f32(z)) * step - vec3<f32>(radius * 0.5) - vec3<f32>(source_offset_x, source_offset_y, source_offset_z);
    let eps = step * 0.22;
    var p = base;
    if (corner == 1u) { p += vec3<f32>(eps, 0.0, 0.0); } else if (corner == 2u) { p += vec3<f32>(0.0, eps, 0.0); }
    return Element(p, vec3<f32>(0.0,0.0,1.0), vec2<f32>(0.0), vec4<f32>(0.0));
}
