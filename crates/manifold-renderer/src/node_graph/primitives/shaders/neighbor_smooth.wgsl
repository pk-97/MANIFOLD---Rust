// node.neighbor_smooth — 5-point cross-neighborhood smoothing over
// an Array<InstanceTransform> with grid topology. Extracted from
// generators/shaders/digital_plants_smooth.wgsl.
//
// Smooths the xyz position component of each instance with its 4
// grid neighbors. Border instances fall back to their own position
// for missing neighbors, preserving edge geometry. The scale (.w)
// and rotation (.rot_pad) are passed through unchanged.

struct SmoothUniforms {
    grid_size: u32,
    instance_count: u32,
    center_weight: f32,
    _pad0: u32,
};

struct InstanceTransform {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: SmoothUniforms;
@group(0) @binding(1) var<storage, read> input: array<InstanceTransform>;
@group(0) @binding(2) var<storage, read_write> output: array<InstanceTransform>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.instance_count { return; }

    let col = idx % u.grid_size;
    let row = idx / u.grid_size;

    // Safe neighbor indices — fall back to self at grid borders.
    let left_idx  = select(idx, idx - 1u,           col > 0u);
    let right_idx = select(idx, idx + 1u,           col < u.grid_size - 1u);
    let down_idx  = select(idx, idx - u.grid_size,  row > 0u);
    let up_idx    = select(idx, idx + u.grid_size,  row < u.grid_size - 1u);

    let pos_c = input[idx].pos_scale.xyz;
    let pos_l = input[left_idx].pos_scale.xyz;
    let pos_r = input[right_idx].pos_scale.xyz;
    let pos_d = input[down_idx].pos_scale.xyz;
    let pos_u = input[up_idx].pos_scale.xyz;

    // Center weight + uniform neighbor weight = ((1 - center) / 4) each.
    let cw = clamp(u.center_weight, 0.0, 1.0);
    let nw = (1.0 - cw) * 0.25;
    let smoothed = pos_c * cw + (pos_l + pos_r + pos_d + pos_u) * nw;

    output[idx] = InstanceTransform(
        vec4<f32>(smoothed, input[idx].pos_scale.w),
        input[idx].rot_pad,
    );
}
