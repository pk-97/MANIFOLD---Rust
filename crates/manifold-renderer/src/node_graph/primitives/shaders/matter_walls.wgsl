// The matter lattice's closed walls (GPU_MPM_SOLVER_DESIGN.md D5), declared
// through `wgsl_includes`. Specific to the MPM lattice: the authored face
// sits on node 3, after the three padding nodes.

// Closed faces (bits −X, +X, −Y, +Y, −Z, +Z) stop velocity into the wall on the
// face node (node 3) and the three padding nodes beyond it, frictionless
// (taichi_elements grid_bounding_box).
fn matter_wall_stop(v: vec3<f32>, coord: vec3<u32>, n: vec3<u32>, faces: u32) -> vec3<f32> {
    let low_closed = vec3<bool>((faces & 1u) != 0u, (faces & 4u) != 0u, (faces & 16u) != 0u);
    let high_closed = vec3<bool>((faces & 2u) != 0u, (faces & 8u) != 0u, (faces & 32u) != 0u);
    let stop_low = low_closed & (coord < vec3<u32>(4u)) & (v < vec3<f32>(0.0));
    let stop_high = high_closed & (coord >= n - vec3<u32>(4u)) & (v > vec3<f32>(0.0));
    return select(v, vec3<f32>(0.0), stop_low | stop_high);
}
