// node.neighbor_smooth — fusable BUFFER body (freeze §12, buffer domain).
// 5-point cross-neighbourhood smoothing over an Array<InstanceTransform> laid
// out as an NxN grid. Smooths the xyz position with the 4 grid neighbours;
// scale (.w) and rotation (.rot) pass through unchanged. Border instances fall
// back to self for missing neighbours. GATHER form — the body reads arbitrary
// neighbour elements from the input array global `buf_in` and computes its own
// indices, so it is a fusion boundary (standalone single-source only).
//
// ABI (buffer standalone codegen): the input array port `in` is bound as the
// global `buf_in: array<Element>`, where the codegen synthesizes
//   struct Element { pos_scale: vec4<f32>, rot: vec4<f32> }
// from InstanceTransform's Channels signature. `count` is the live element
// count (= instance_count); unused here. `grid_size` arrives as i32 (Int param)
// and is cast to u32 to match the hand shader's u32 grid arithmetic exactly.
// Matches neighbor_smooth.wgsl (the parity oracle).
fn body(idx: u32, count: u32, grid_size: i32, center_weight: f32) -> Element {
    let gs = u32(grid_size);
    let col = idx % gs;
    let row = idx / gs;

    // Safe neighbour indices — fall back to self at grid borders.
    let left_idx  = select(idx, idx - 1u,  col > 0u);
    let right_idx = select(idx, idx + 1u,  col < gs - 1u);
    let down_idx  = select(idx, idx - gs,  row > 0u);
    let up_idx    = select(idx, idx + gs,  row < gs - 1u);

    let pos_c = buf_in[idx].pos_scale.xyz;
    let pos_l = buf_in[left_idx].pos_scale.xyz;
    let pos_r = buf_in[right_idx].pos_scale.xyz;
    let pos_d = buf_in[down_idx].pos_scale.xyz;
    let pos_u = buf_in[up_idx].pos_scale.xyz;

    // Center weight + uniform neighbour weight = ((1 - center) / 4) each.
    let cw = clamp(center_weight, 0.0, 1.0);
    let nw = (1.0 - cw) * 0.25;
    let smoothed = pos_c * cw + (pos_l + pos_r + pos_d + pos_u) * nw;

    return Element(
        vec4<f32>(smoothed, buf_in[idx].pos_scale.w),
        buf_in[idx].rot,
    );
}
