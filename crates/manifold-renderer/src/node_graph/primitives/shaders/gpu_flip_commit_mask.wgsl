struct MaskParams {
    words: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

struct ClockPlan {
    step_dt: f32,
    elapsed: f32,
    remaining: f32,
    maximum_speed: f32,
    cap_hit: u32,
    nonfinite: u32,
    step_index: u32,
    event: u32,
    numerical_end: f32,
    marker_limit: f32,
    _pad0: u32,
    live_mode: u32,
};

@group(0) @binding(0) var<uniform> params: MaskParams;
@group(0) @binding(1) var<storage, read> plan: array<ClockPlan>;
@group(0) @binding(2) var<storage, read_write> destination: array<u32>;
@group(0) @binding(3) var<storage, read_write> saved: array<u32>;

@compute @workgroup_size(256)
fn commit_mask(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.words || plan[0].live_mode == 0u {
        return;
    }
    if plan[0].step_dt > 0.0 {
        saved[idx] = destination[idx];
    } else {
        destination[idx] = saved[idx];
    }
}
