// Header: active count, indirect grid x/y/z, brick dimensions x/y/z,
// reserved. Then N occupancy words and N compact-list slots.
fn liquid_brick_active(idx: u32, dims: vec3<u32>) -> bool {
    if idx >= dims.x * dims.y * dims.z { return false; }
    let p = vec3<u32>(idx % dims.x, (idx / dims.x) % dims.y, idx / (dims.x * dims.y));
    let b = p / 8u;
    return buf_bricks[8u + b.x + buf_bricks[4u] * (b.y + buf_bricks[5u] * b.z)] != 0u;
}

// Padding maps to a sentinel rejected before coincident reads/output writes.
fn liquid_brick_map(invocation: u32, dims: vec3<u32>) -> u32 {
    let rank = invocation / 512u;
    if rank >= buf_bricks[0u] { return 0xffffffffu; }
    let bx = buf_bricks[4u];
    let by = buf_bricks[5u];
    let n = bx * by * buf_bricks[6u];
    let brick = buf_bricks[8u + n + rank];
    let b = vec3<u32>(brick % bx, (brick / bx) % by, brick / (bx * by));
    let local = invocation % 512u;
    let p = b * 8u + vec3<u32>(local % 8u, (local / 8u) % 8u, local / 64u);
    if any(p >= dims) { return 0xffffffffu; }
    return p.x + dims.x * (p.y + dims.y * p.z);
}

fn liquid_brick_select(invocation: u32, dims: vec3<u32>, mode: u32) -> u32 {
    if mode == 0u { return invocation; }
    if mode == 1u { return liquid_brick_map(invocation, dims); }
    if liquid_brick_active(invocation, dims) { return 0xffffffffu; }
    return invocation;
}

fn liquid_cell_brick_index(invocation: u32) -> u32 {
    let nodes = vec3<u32>(vec3<f32>(params.nodes_x, params.nodes_y, params.nodes_z));
    let dims = nodes - vec3<u32>(1u);
    if params.brick_pass != 1u { return invocation; }
    return liquid_brick_map(invocation, dims);
}
