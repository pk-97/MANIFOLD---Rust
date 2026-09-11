// Original particle indices are stable canonical component identities.
fn body(idx: u32, count: u32) -> u32 {
    let p = buf_particles[idx];
    let nonfinite = any((bitcast<vec4<u32>>(p.position_mass) & vec4<u32>(0x7f800000u)) == vec4<u32>(0x7f800000u));
    let outside = any(p.position_mass.xyz < vec3<f32>(-2.0, 0.0, -2.0)) || any(p.position_mass.xyz >= vec3<f32>(2.0, 4.0, 2.0));
    if (p.position_mass.w <= 0.0 || nonfinite || outside) { return 0xffffffffu; }
    return idx;
}
