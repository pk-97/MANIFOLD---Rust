// node.matter_to_particles — MatterPoint to FluidParticle, index for index
// (GPU_MPM_SOLVER_DESIGN.md D6: the cell sort reads the seam's record). Radius
// is the sphere of the point's rest volume, matter_frame's rule. A removed
// point (id 0) or one with a non-finite position gets radius 0, the seam's
// unused-slot mark, so sorts and draws skip it.
fn m2p_finite3(v: vec3<f32>) -> bool {
    let e = vec3<u32>(bitcast<u32>(v.x), bitcast<u32>(v.y), bitcast<u32>(v.z)) & vec3<u32>(0x7f800000u);
    return all(e != vec3<u32>(0x7f800000u));
}

fn body(idx: u32, count: u32, e_points: Element) -> Element2 {
    if e_points.id == 0u || !m2p_finite3(e_points.position) {
        return Element2(vec4<f32>(0.0), vec3<f32>(0.0), 0u);
    }
    let radius = 0.6203505 * pow(max(e_points.affine_y.w, 0.0), 1.0 / 3.0);
    return Element2(vec4<f32>(e_points.position, radius), e_points.velocity, e_points.id);
}
