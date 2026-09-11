fn body(uv: vec3<f32>, dims: vec3<f32>, vol_res: i32, vol_depth: i32, radius: f32) -> vec4<f32> {
    let world = vec3<f32>(-2.0, 0.0, -2.0) + uv * 4.0;
    let r = clamp(radius, 0.0625, 0.125);
    let cell = vec3<i32>(floor((world - vec3<f32>(-2.0, 0.0, -2.0)) / 0.125));
    if (any(cell < vec3<i32>(0)) || any(cell >= vec3<i32>(vec3<u32>(32u)))) { return vec4<f32>(0.0); }
    let bins = vec3<i32>(ceil(r / 0.125));
    let lo = max(cell - bins, vec3<i32>(0));
    let hi = min(cell + bins, vec3<i32>(31));
    let norm = 315.0 / (64.0 * 3.14159265359 * r * r * r);
    var total = 0.0; var foam_sum = 0.0;
    for (var z = lo.z; z <= hi.z; z++) { for (var y = lo.y; y <= hi.y; y++) { for (var x = lo.x; x <= hi.x; x++) {
        let bin = u32(x + 32 * (y + 32 * z)); var link = buf_heads[bin]; var guard = 0u;
        loop { if (link == 0u || guard >= arrayLength(&buf_particles)) { break; } let idx = link - 1u; if (idx >= arrayLength(&buf_particles)) { break; } let p = buf_particles[idx]; let d = world - p.position_mass.xyz; let q = dot(d,d) / (r*r); if (q < 1.0) { let w = p.position_mass.w / 1000.0 * norm * ((1.0-q)*(1.0-q)*(1.0-q)); total += w; foam_sum += w * buf_foam[idx]; } link = buf_next[idx]; guard++; }
    } } }
    return vec4<f32>(total, foam_sum / max(total, 1e-20), 0.0, 0.0);
}
