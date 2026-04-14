// Digital Plants — Data smoothing: 5-point cross neighbor average on instance positions.
//
// Reads raw instance positions from the compute pass and writes smoothed
// positions to a separate output buffer.  The grid topology (400x400) is used
// to identify spatial neighbors.  Border instances fall back to their own
// position for missing neighbors, preserving edge geometry.

struct SmoothUniforms {
    grid_size: u32,
    instance_count: u32,
    _pad0: u32,
    _pad1: u32,
};

struct Instance {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: SmoothUniforms;
@group(0) @binding(1) var<storage, read> input: array<Instance>;
@group(0) @binding(2) var<storage, read_write> output: array<Instance>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.instance_count { return; }

    let col = idx % u.grid_size;
    let row = idx / u.grid_size;

    // Safe neighbor indices — fall back to self at grid borders.
    let left_idx  = select(idx, idx - 1u,          col > 0u);
    let right_idx = select(idx, idx + 1u,          col < u.grid_size - 1u);
    let down_idx  = select(idx, idx - u.grid_size, row > 0u);
    let up_idx    = select(idx, idx + u.grid_size, row < u.grid_size - 1u);

    let pos_c = input[idx].pos_scale.xyz;
    let pos_l = input[left_idx].pos_scale.xyz;
    let pos_r = input[right_idx].pos_scale.xyz;
    let pos_d = input[down_idx].pos_scale.xyz;
    let pos_u = input[up_idx].pos_scale.xyz;

    // Weighted average: center 60%, each neighbor 10%.
    let smoothed = pos_c * 0.6 + (pos_l + pos_r + pos_d + pos_u) * 0.1;

    // Copy scale and rotation unchanged from input.
    output[idx] = Instance(
        vec4<f32>(smoothed, input[idx].pos_scale.w),
        input[idx].rot_pad,
    );
}
