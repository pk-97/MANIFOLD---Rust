// GPU-local FLIP clock.  This is a scheduler helper, not a catalog atom.
//
// Speed reduction follows FLIP Fluids' FluidSimulation::_calculateNextTimeStep
// (fluidsimulation.cpp:11088-11194).  Scheduling follows nextUpdateTimeStep
// (fluidsimulation.cpp:11430-11470): the last allowed substep takes all
// remaining frame time.  The externally-stepped throw is deliberately absent.
// Ported engine code: Ryan L. Guy and Dennis Fassbaender, MIT.
// See THIRD_PARTY_NOTICES.md.

struct ReduceParams {
    count: u32,
    mode: u32,
    _pad0: u32,
    _pad1: u32,
};

struct ClockParams {
    frame_duration: f32,
    cell_size: f32,
    cfl: f32,
    surface_condition: f32,
    surface_constant: f32,
    color_mixing_rate: f32,
    _pad_prediction: f32,
    _pad0: f32,
    min_frame_steps: u32,
    max_frame_steps: u32,
    flags: u32,
    _pad1: u32,
    constant_force: vec4<f32>,
};

struct BodyVertex {
    position: vec4<f32>,
    velocity: vec4<f32>,
    acceleration: vec4<f32>,
    angular_velocity: vec4<f32>,
    angular_acceleration: vec4<f32>,
    centroid: vec4<f32>,
};

struct Plan {
    step_dt: f32,
    elapsed: f32,
    remaining: f32,
    maximum_speed: f32,
    cap_hit: u32,
    nonfinite: u32,
    step_index: u32,
    _pad: u32,
};

struct FluidParticle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
};

@group(0) @binding(0) var<storage, read> marker_values: array<FluidParticle>;
@group(0) @binding(1) var<storage, read_write> marker_partials: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> marker_reduce_params: ReduceParams;

var<workgroup> speeds: array<f32, 64>;
var<workgroup> invalids: array<u32, 64>;

fn finite3(v: vec3<f32>) -> bool {
    return finite1(v.x) && finite1(v.y) && finite1(v.z);
}

fn reduce_group(lid: u32, count: u32, value: f32, bad: u32) -> vec4<f32> {
    speeds[lid] = value;
    invalids[lid] = bad;
    workgroupBarrier();
    var width = 32u;
    loop {
        if (lid < width) {
            speeds[lid] = max(speeds[lid], speeds[lid + width]);
            invalids[lid] = max(invalids[lid], invalids[lid + width]);
        }
        workgroupBarrier();
        if (width == 1u) { break; }
        width = width / 2u;
    }
    return vec4<f32>(speeds[0], f32(invalids[0]), f32(count), 0.0);
}

@compute @workgroup_size(64)
fn reduce_marker(@builtin(local_invocation_id) local: vec3<u32>,
                 @builtin(workgroup_id) group: vec3<u32>) {
    let lid = local.x;
    let gid = group.x;
    let idx = gid * 64u + lid;
    var speed = 0.0;
    var bad = 0u;
    if (idx < marker_reduce_params.count) {
        let particle = marker_values[idx];
        let v = particle.velocity;
        if (!finite1(particle.position_radius.w)) {
            bad = 1u;
        } else if (particle.position_radius.w > 0.0 && finite3(v) && finite3(particle.position_radius.xyz)) {
            speed = length(v);
            if (!finite1(speed)) { speed = 0.0; bad = 1u; }
        } else if (particle.position_radius.w > 0.0) {
            bad = 1u;
        }
    }
    let reduced = reduce_group(lid, marker_reduce_params.count, speed, bad);
    if (lid == 0u) { marker_partials[group.x] = reduced; }
}

@group(0) @binding(3) var<storage, read> body_values: array<BodyVertex>;
@group(0) @binding(4) var<storage, read_write> body_partials: array<vec4<f32>>;
@group(0) @binding(5) var<uniform> body_reduce_params: ReduceParams;
@group(0) @binding(6) var<uniform> body_clock_params: ClockParams;

fn body_speed(body: BodyVertex, source: bool) -> f32 {
    let dt = body_clock_params.frame_duration;
    let r = body.position.xyz - body.centroid.xyz;
    let initial = body.velocity.xyz + cross(body.angular_velocity.xyz, r);
    if (source) {
        return length(initial);
    }
    // RigidFluidCoupling::pointSpeed: affine point velocity is bounded by
    // both endpoint norms, including a decelerating body.
    let acceleration = body.acceleration.xyz + cross(body.angular_acceleration.xyz, r);
    return max(length(initial), length(initial + dt * acceleration));
}

@compute @workgroup_size(64)
fn reduce_body(@builtin(local_invocation_id) local: vec3<u32>,
               @builtin(workgroup_id) group: vec3<u32>) {
    let lid = local.x;
    let gid = group.x;
    let idx = gid * 64u + lid;
    var speed = 0.0;
    var bad = 0u;
    if (idx < body_reduce_params.count) {
        let body = body_values[idx];
        if (body.position.w != 0.0) {
            if (finite3(body.position.xyz) && finite3(body.centroid.xyz) &&
                finite3(body.velocity.xyz) && finite3(body.angular_velocity.xyz) &&
                finite3(body.acceleration.xyz) && finite3(body.angular_acceleration.xyz)) {
                speed = body_speed(body, body_reduce_params.mode != 0u);
                if (!finite1(speed)) { speed = 0.0; bad = 1u; }
            } else {
                bad = 1u;
            }
        }
    }
    let reduced = reduce_group(lid, body_reduce_params.count, speed, bad);
    if (lid == 0u) { body_partials[group.x] = reduced; }
}

@group(0) @binding(8) var<storage, read> partial_values: array<vec4<f32>>;
@group(0) @binding(9) var<storage, read_write> partial_output: array<vec4<f32>>;
@group(0) @binding(10) var<uniform> partial_params: ReduceParams;

@compute @workgroup_size(64)
fn reduce_partial(@builtin(local_invocation_id) local: vec3<u32>,
                  @builtin(workgroup_id) group: vec3<u32>) {
    let lid = local.x;
    let gid = group.x;
    let idx = gid * 64u + lid;
    var speed = 0.0;
    var bad = 0u;
    if (idx < partial_params.count) {
        let v = partial_values[idx];
        speed = v.x;
        bad = u32(v.y != 0.0);
    }
    speeds[lid] = speed;
    invalids[lid] = bad;
    workgroupBarrier();
    var width = 32u;
    loop {
        if (lid < width) {
            speeds[lid] = max(speeds[lid], speeds[lid + width]);
            invalids[lid] = max(invalids[lid], invalids[lid + width]);
        }
        workgroupBarrier();
        if (width == 1u) { break; }
        width = width / 2u;
    }
    if (lid == 0u) {
        partial_output[gid] = vec4<f32>(speeds[0], f32(invalids[0]), f32(partial_params.count), 0.0);
    }
}

@group(0) @binding(11) var<storage, read> marker_result: array<vec4<f32>>;
@group(0) @binding(12) var<storage, read> obstacle_result: array<vec4<f32>>;
@group(0) @binding(13) var<storage, read> source_result: array<vec4<f32>>;
@group(0) @binding(14) var<storage, read_write> output_plan: array<Plan>;
@group(0) @binding(15) var<uniform> clock: ClockParams;

const FIRST_SUBSTEP: u32 = 1u;
const FLUID_PRESENT_OR_GENERATING: u32 = 2u;
const SURFACE_TENSION: u32 = 4u;
const COLOR_MIXING: u32 = 8u;
const EPSILON: f32 = 1.0e-6;

fn finite1(x: f32) -> bool {
    return ((bitcast<u32>(x) >> 23u) & 255u) != 255u;
}

@compute @workgroup_size(1)
fn begin_frame() {
    let valid = finite1(clock.frame_duration) && clock.frame_duration > 0.0;
    let duration = select(0.0, clock.frame_duration, valid);
    output_plan[0] = Plan(0.0, 0.0, duration, 0.0, 0u, u32(!valid), 0u, 0u);
}

@compute @workgroup_size(1)
fn schedule() {
    let state = output_plan[0];
    if (state.remaining == 0.0) {
        output_plan[0].step_dt = 0.0;
        return;
    }
    var marker_speed = marker_result[0].x;
    var obstacle_speed = obstacle_result[0].x;
    var source_speed = source_result[0].x;
    var invalid = marker_result[0].y != 0.0 || obstacle_result[0].y != 0.0 || source_result[0].y != 0.0;
    if (!finite1(clock.frame_duration) || !finite1(state.remaining) ||
        !finite1(clock.cell_size) || !finite1(clock.cfl) ||
        state.remaining <= 0.0 || clock.cell_size <= 0.0 || clock.cfl < 1.0) {
        invalid = true;
    }
    if (invalid) {
        // A non-finite state is visible to the HUD, but never stops the show.
        // The finite frame duration is the continuation when the cursor was
        // corrupted; no speed or quality heuristic is invented.
        let safe_remaining = select(state.remaining, clock.frame_duration, !finite1(state.remaining));
        output_plan[0] = Plan(
            safe_remaining,
            state.elapsed + safe_remaining,
            max(state.remaining - safe_remaining, 0.0),
            0.0,
            state.cap_hit,
            1u,
            state.step_index + 1u,
            0u,
        );
        return;
    }
    if ((clock.flags & FIRST_SUBSTEP) != 0u && state.step_index == 0u) {
        // FLIP Fluids predicts source velocity first, then adds the norm of
        // the constant body force once. Adding the vector to each source
        // vertex would incorrectly allow cancellation between samples.
        marker_speed = source_speed + length(clock.constant_force.xyz) * clock.frame_duration;
    }
    var maximum_speed = marker_speed;
    if ((clock.flags & FLUID_PRESENT_OR_GENERATING) != 0u) {
        maximum_speed = max(maximum_speed, obstacle_speed);
    }
    var limit = clock.cfl * clock.cell_size / (maximum_speed + EPSILON);
    if ((clock.flags & SURFACE_TENSION) != 0u && (clock.flags & FLUID_PRESENT_OR_GENERATING) != 0u) {
        let restriction = clock.surface_condition * sqrt(clock.cell_size * clock.cell_size * clock.cell_size) *
            sqrt(1.0 / (clock.surface_constant + EPSILON));
        limit = min(limit, restriction);
    }
    if ((clock.flags & COLOR_MIXING) != 0u) {
        limit = min(limit, 1.0 / (clock.color_mixing_rate + EPSILON));
    }
    let count = max(ceil(clock.frame_duration / limit), 1.0);
    var dt = min(state.remaining, clock.frame_duration / count);
    let substep_time = clock.frame_duration / f32(max(clock.min_frame_steps, 1u));
    let time_completed = clock.frame_duration - state.remaining;
    let step_limit = f32(state.step_index + 1u) * substep_time;
    if (time_completed + dt > step_limit) {
        dt = min(substep_time, state.remaining);
    }
    // FLIP Fluids' internal final-step rule: the last allowed substep takes
    // ALL remaining frame time, bending CFL for that one step.
    var cap_hit = 0u;
    if (state.step_index + 1u >= max(clock.max_frame_steps, 1u)) {
        dt = state.remaining;
        cap_hit = 1u;
    }
    output_plan[0] = Plan(
        dt,
        select(state.elapsed + dt, clock.frame_duration, dt == state.remaining),
        max(state.remaining - dt, 0.0),
        maximum_speed,
        max(state.cap_hit, cap_hit),
        state.nonfinite,
        state.step_index + 1u,
        0u,
    );
}
