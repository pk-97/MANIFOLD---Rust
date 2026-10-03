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
    interval_sequence: u32,
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

struct CoupledBody {
    position_inv_mass: vec4<f32>,
    rotation: vec4<f32>,
    linear_velocity: vec4<f32>,
    angular_velocity: vec4<f32>,
    inv_inertia_x: vec4<f32>,
    inv_inertia_y: vec4<f32>,
    inv_inertia_z: vec4<f32>,
    accel_shape: vec4<f32>,
};

struct Plan {
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

struct EventFieldParams {
    origin_spacing: vec4<f32>,
    nodes_stride: vec4<u32>,
};

struct FluidParticle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
};

@group(0) @binding(0) var<storage, read> marker_values: array<FluidParticle>;
@group(0) @binding(1) var<storage, read_write> marker_partials: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> marker_reduce_params: ReduceParams;
@group(0) @binding(7) var<storage, read_write> marker_histogram_atomic: array<atomic<u32>>;
@group(0) @binding(20) var<storage, read_write> marker_outliers_atomic: array<atomic<u32>>;

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
        var v = particle.velocity;
        let event_index = current_event_index();
        if (event_index != 0xffffffffu) {
            v = v + event_impulse_at(particle.position_radius.xyz, event_index);
        }
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

// Port of FluidSimulation::_getMarkerParticleSpeedLimit's histogram and
// relative-outlier counts. Classification is a separate ordered pass because
// the thresholds depend on the reduced maximum marker speed.
@compute @workgroup_size(64)
fn classify_marker(@builtin(global_invocation_id) gid: vec3<u32>) {
    if marker_clock_plan[0].live_mode == 0u || marker_clock_plan[0].step_dt <= 0.0 {
        return;
    }
    let idx = gid.x;
    if idx >= marker_reduce_params.count {
        return;
    }
    let particle = marker_values[idx];
    let v = particle.velocity;
    if !(particle.position_radius.w > 0.0 && finite3(v) && finite3(particle.position_radius.xyz)) {
        return;
    }
    let speed = length(v);
    if !finite1(speed) {
        return;
    }
    let speed_limit_step = marker_clock_params.cfl * marker_clock_params.cell_size /
        marker_clock_params.frame_duration;
    let bin = min(u32(floor(speed / speed_limit_step)),
        max(marker_clock_params.max_frame_steps, 1u) - 1u);
    atomicAdd(&marker_histogram_atomic[bin], 1u);
    let maximum = partial_values[0].x;
    if speed >= 0.90 * maximum && speed < 0.99999 * maximum {
        atomicAdd(&marker_outliers_atomic[0], 1u);
    }
    if speed >= 0.99999 * maximum {
        atomicAdd(&marker_outliers_atomic[1], 1u);
    }
}

// Finish the native _getMarkerParticleSpeedLimit policy after the scheduler
// has selected this numerical interval. Native removal uses the whole
// accepted frame duration (_currentFrameDeltaTime), even at a CFL/event split.
// The limit is consumed by the
// particle write pass after advection; it never changes the CFL maximum.
@compute @workgroup_size(1)
fn finalize_marker_limit() {
    let state = output_plan[0];
    if state.live_mode == 0u || state.step_dt <= 0.0 {
        return;
    }
    let speed_limit_step = clock.cfl * clock.cell_size / clock.frame_duration;
    let max_steps = max(clock.max_frame_steps, 1u);
    var live_count = 0u;
    for (var b = 0u; b < max_steps; b = b + 1u) {
        live_count = live_count + marker_histogram_words[b];
    }
    let max_removal = min(u32(f32(live_count) * 0.0005), 35u);
    var maxspeed = f32(max_steps) * speed_limit_step;
    var removed = 0u;
    for (var bin = max_steps; bin > 1u; bin = bin - 1u) {
        let count = marker_histogram_words[bin - 1u];
        if (removed + count > max_removal) {
            break;
        }
        removed = removed + count;
        maxspeed = f32(max(bin - 1u + 4u, max_steps)) * speed_limit_step;
    }
    let lower_outliers = marker_outlier_counts[0];
    let outliers = marker_outlier_counts[1];
    if outliers <= 6u && lower_outliers <= 6u {
        maxspeed = min(maxspeed, 0.99999 * marker_result[0].x);
    }
    output_plan[0].marker_limit = max(maxspeed, f32(max_steps) * speed_limit_step);
}

@compute @workgroup_size(64)
fn remove_marker_particles(@builtin(global_invocation_id) gid: vec3<u32>) {
    if marker_clock_plan[0].live_mode == 0u || marker_clock_plan[0].step_dt <= 0.0 {
        return;
    }
    let idx = gid.x;
    if idx >= marker_reduce_params.count {
        return;
    }
    let particle = marker_values_rw[idx];
    if !(particle.position_radius.w > 0.0 && finite3(particle.velocity)) {
        return;
    }
    let limit = marker_clock_plan[0].marker_limit;
    if limit > 0.0 && dot(particle.velocity, particle.velocity) > limit * limit {
        marker_values_rw[idx].position_radius.w = 0.0;
    }
}

@group(0) @binding(3) var<storage, read> body_values: array<BodyVertex>;
@group(0) @binding(4) var<storage, read_write> body_partials: array<vec4<f32>>;
@group(0) @binding(5) var<uniform> body_reduce_params: ReduceParams;
@group(0) @binding(6) var<uniform> body_clock_params: ClockParams;
// The runtime binds the current tick's final body rows with a byte offset;
// coupled hull vertices carry the relative row index plus one in velocity.w.
@group(0) @binding(28) var<storage, read> clock_bodies: array<CoupledBody>;
@group(0) @binding(29) var<storage, read> clock_reaction: array<f32>;

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

// Match solid_face_velocity: current pose, external acceleration and the
// pressure reaction all contribute to the affine point velocity used by CFL.
fn coupled_body_speed(body: BodyVertex) -> vec2<f32> {
    let row_index = u32(body.velocity.w) - 1u;
    let coupled = clock_bodies[row_index];
    let elapsed = marker_clock_plan[0].elapsed;
    let centre = coupled.position_inv_mass.xyz + coupled.linear_velocity.xyz * elapsed;
    let vertex = body.position.xyz + coupled.linear_velocity.xyz * elapsed;
    let r = vertex - centre;
    var linear = coupled.linear_velocity.xyz;
    var angular = coupled.angular_velocity.xyz;
    let angular_acceleration = vec3<f32>(
        coupled.inv_inertia_x.w, coupled.inv_inertia_y.w, coupled.inv_inertia_z.w);
    if coupled.position_inv_mass.w > 0.0 {
        let reaction = 8u * row_index;
        let push = vec3<f32>(
            clock_reaction[reaction], clock_reaction[reaction + 1u], clock_reaction[reaction + 2u]);
        let turn = vec3<f32>(
            clock_reaction[reaction + 4u], clock_reaction[reaction + 5u], clock_reaction[reaction + 6u]);
        linear = fma(coupled.accel_shape.xyz, vec3<f32>(elapsed), linear) +
            coupled.position_inv_mass.w * push;
        angular = fma(angular_acceleration, vec3<f32>(elapsed), angular) + vec3<f32>(
            dot(coupled.inv_inertia_x.xyz, turn),
            dot(coupled.inv_inertia_y.xyz, turn),
            dot(coupled.inv_inertia_z.xyz, turn));
    }
    let initial = linear + cross(angular, r);
    let acceleration = coupled.accel_shape.xyz + cross(angular_acceleration, r);
    let endpoint = initial + body_clock_params.frame_duration * acceleration;
    let speed = max(length(initial), length(endpoint));
    return vec2<f32>(speed, select(0.0, 1.0, !finite1(speed)));
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
                if (body_reduce_params.mode == 0u && body.velocity.w > 0.0 && finite1(body.velocity.w)) {
                    let coupled_speed = coupled_body_speed(body);
                    speed = coupled_speed.x;
                    bad = u32(coupled_speed.y != 0.0);
                } else {
                    speed = body_speed(body, body_reduce_params.mode != 0u);
                }
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
@group(0) @binding(22) var<uniform> marker_clock_params: ClockParams;
@group(0) @binding(24) var<storage, read> marker_clock_plan: array<Plan>;
@group(0) @binding(25) var<storage, read_write> marker_values_rw: array<FluidParticle>;
@group(0) @binding(26) var<uniform> event_field: EventFieldParams;
@group(0) @binding(27) var<storage, read> event_impulses: array<f32>;

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
// Packed as four f32 words by the graph seam. Word 0 is seconds relative to
// the current interval and word 1 is the impulse-lattice index bitcast as u32.
@group(0) @binding(16) var<storage, read> live_hits: array<vec4<f32>>;
@group(0) @binding(17) var<uniform> live_hit_count: vec4<u32>;
@group(0) @binding(18) var<storage, read> marker_histogram_words: array<u32>;
@group(0) @binding(19) var<storage, read> marker_outlier_counts: array<u32>;

struct EventCorner {
    index: u32,
    weight: f32,
};

fn event_corner(x: vec3<f32>, k: u32) -> EventCorner {
    let dims = event_field.nodes_stride.xyz;
    let g = (x - event_field.origin_spacing.xyz) / event_field.origin_spacing.w;
    let base = min(max(floor(g), vec3<f32>(0.0)), vec3<f32>(dims - vec3<u32>(2u)));
    let f = clamp(g - base, vec3<f32>(0.0), vec3<f32>(1.0));
    let o = vec3<u32>(k & 1u, (k >> 1u) & 1u, (k >> 2u) & 1u);
    let c = vec3<u32>(base) + o;
    let w = select(vec3<f32>(1.0) - f, f, o == vec3<u32>(1u));
    return EventCorner(c.x + dims.x * (c.y + dims.y * c.z), w.x * w.y * w.z);
}

fn current_event_index() -> u32 {
    let state = marker_clock_plan[0];
    if state.live_mode == 0u || live_hit_count.x == 0u ||
        event_field.nodes_stride.w == 0u || any(event_field.nodes_stride.xyz < vec3<u32>(2u)) {
        return 0xffffffffu;
    }
    let consumed = state.event & 0x7fffffffu;
    let consumed_valid = (state.event & 0x80000000u) != 0u;
    for (var i = 0u; i < live_hit_count.x; i = i + 1u) {
        let hit = live_hits[i];
        if (bitcast<u32>(hit.z) != clock.interval_sequence) { continue; }
        if (finite1(hit.x) && hit.x == state.elapsed) {
            let index = bitcast<u32>(hit.y);
            if (!consumed_valid || index != consumed) {
                return index;
            }
        }
    }
    return 0xffffffffu;
}

fn event_impulse_at(x: vec3<f32>, event_index: u32) -> vec3<f32> {
    let stride = event_field.nodes_stride.w;
    var value = vec3<f32>(0.0);
    let base = event_index * stride;
    for (var k = 0u; k < 8u; k = k + 1u) {
        let corner = event_corner(x, k);
        let offset = base + corner.index * 4u;
        value = value + corner.weight * vec3<f32>(
            event_impulses[offset], event_impulses[offset + 1u], event_impulses[offset + 2u]);
    }
    return value;
}

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
    output_plan[0] = Plan(0.0, 0.0, duration, 0.0, 0u, u32(!valid), 0u, 0u,
        0.0, 0.0, 0u, 1u);
}

@compute @workgroup_size(1)
fn schedule() {
    let state = output_plan[0];
    if (state.remaining == 0.0) {
        output_plan[0].step_dt = 0.0;
        return;
    }
    var marker_speed = marker_result[0].x;
    let marker_nonfinite = marker_result[0].y != 0.0;
    if (marker_nonfinite) {
        marker_speed = 0.0;
    }
    var obstacle_speed = obstacle_result[0].x;
    var source_speed = source_result[0].x;
    var invalid = obstacle_result[0].y != 0.0 || source_result[0].y != 0.0;
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
            state.elapsed + safe_remaining,
            0.0,
            0u,
            state.live_mode,
        );
        return;
    }
    // A live event is a boundary on the current GPU cursor. The segment that
    // reaches it is shortened, and the following segment carries the packed
    // one-shot selection in Plan._pad. Event segments do not consume the
    // numerical-step budget used by the CFL/max-step rule.
    var event_word = 0u;
    var event_at_start = false;
    let previous_event = state.event & 0x7fffffffu;
    let previous_event_valid = (state.event & 0x80000000u) != 0u;
    var next_event = clock.frame_duration + 1.0;
    for (var i = 0u; i < live_hit_count.x; i = i + 1u) {
        let hit = live_hits[i];
        if (bitcast<u32>(hit.z) != clock.interval_sequence) { continue; }
        if (!finite1(hit.x) || hit.x < 0.0 || hit.x > clock.frame_duration) {
            continue;
        }
        let event_index = bitcast<u32>(hit.y);
        let already_applied = previous_event_valid && event_index == previous_event;
        if (hit.x == state.elapsed && !event_at_start && !already_applied) {
            event_word = 0x80000000u | bitcast<u32>(hit.y);
            event_at_start = true;
        } else if (hit.x > state.elapsed) {
            next_event = min(next_event, hit.x);
        }
    }
    if ((clock.flags & FIRST_SUBSTEP) != 0u && state.step_index == 0u) {
        // FLIP Fluids predicts source velocity first, then adds the norm of
        // the constant body force once. Preserve an event boost at the same
        // first cursor while retaining the native source override otherwise.
        let predicted_source = source_speed + length(clock.constant_force.xyz) * clock.frame_duration;
        marker_speed = select(predicted_source, max(marker_speed, predicted_source), event_at_start);
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
    let pending = state.numerical_end > state.elapsed;
    let substep_time = clock.frame_duration / f32(max(clock.min_frame_steps, 1u));
    let step_limit = f32(state.step_index + 1u) * substep_time;
    if (!pending && state.elapsed + dt > step_limit) {
        dt = min(substep_time, state.remaining);
    }
    // FLIP Fluids' internal final-step rule: the last allowed substep takes
    // ALL remaining frame time, bending CFL for that one step.
    var cap_hit = 0u;
    if (!pending && state.step_index + 1u >= max(clock.max_frame_steps, 1u)) {
        dt = state.remaining;
        cap_hit = 1u;
    }
    // An interior event is a transport boundary even when the CFL step has
    // reached the cap. The next dispatch continues with the same numerical
    // step budget, so the stretched remainder rule still owns the tail.
    var numerical_end = select(state.elapsed + dt, state.numerical_end, pending);
    if (pending && state.step_index + 1u < max(clock.max_frame_steps, 1u)) {
        let pending_dt = min(state.remaining, clock.frame_duration / count);
        if (pending_dt < numerical_end - state.elapsed) {
            dt = pending_dt;
            numerical_end = state.elapsed + pending_dt;
        } else {
            dt = numerical_end - state.elapsed;
        }
    } else if (pending) {
        dt = numerical_end - state.elapsed;
    }
    var end = state.elapsed + dt;
    if (next_event <= numerical_end) {
        // Preserve the producer's exact f32 boundary. Reconstructing it as
        // elapsed + (boundary - elapsed) can miss identity by one ulp.
        dt = next_event - state.elapsed;
        end = next_event;
    }
    let numerical_complete = end >= numerical_end;
    output_plan[0] = Plan(
        dt,
        select(end, clock.frame_duration, dt == state.remaining),
        max(state.remaining - dt, 0.0),
        maximum_speed,
        max(state.cap_hit, cap_hit),
        max(state.nonfinite, u32(marker_nonfinite)),
        state.step_index + select(0u, 1u, numerical_complete),
        event_word,
        select(numerical_end, 0.0, numerical_complete),
        0.0,
        0u,
        state.live_mode,
    );
}
