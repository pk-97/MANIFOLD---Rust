// FLIP Fluids LevelSetSolver::_stepSolverThreadUpwind, MIT; see notices.
fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32) -> f32 {
    let dims = vec3<u32>(vec3<f32>(nodes_x,nodes_y,nodes_z));
    let d = buf_levelset[idx];
    if idx >= dims.x*dims.y*dims.z { return d; }
    if buf_valid[idx] == 0u { return d; }
    let coord = vec3<u32>(idx % dims.x, idx / dims.x % dims.y, idx / (dims.x*dims.y));
    let strides = vec3<u32>(1u,dims.x,dims.x*dims.y);
    var positive = 0.0;
    var negative = 0.0;
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        var lo = idx; var hi = idx;
        if coord[axis] > 0u { lo = idx - strides[axis]; }
        if coord[axis] + 1u < dims[axis] { hi = idx + strides[axis]; }
        let a = (d-buf_levelset[lo]) / cell_size;
        let b = (buf_levelset[hi]-d) / cell_size;
        positive = positive + max(a,0.0)*max(a,0.0) + min(b,0.0)*min(b,0.0);
        negative = negative + min(a,0.0)*min(a,0.0) + max(b,0.0)*max(b,0.0);
    }
    let sign = d / sqrt(d*d + cell_size*cell_size);
    return d - 0.5*cell_size*max(sign,0.0)*(sqrt(positive)-1.0)
        - 0.5*cell_size*min(sign,0.0)*(sqrt(negative)-1.0);
}
