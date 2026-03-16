// Physarum agent update compute shader.
// Each agent senses the trail map, steers toward highest concentration,
// moves forward, and deposits into an atomic accumulator buffer.

struct PhysarumAgent {
    pos: vec2<f32>,
    angle: f32,
    _pad: f32,
};

struct AgentUniforms {
    agent_count: u32,
    width: u32,
    height: u32,
    sensor_dist: f32,
    sensor_angle: f32,
    rotation_angle: f32,
    step_size: f32,
    deposit_scaled: f32,
    frame_count: u32,
    beat: f32,
    reactivity: f32,
    _pad: f32,
};

@group(0) @binding(0) var<storage, read_write> agents: array<PhysarumAgent>;
@group(0) @binding(1) var trail_tex: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> accum: array<atomic<u32>>;
@group(0) @binding(3) var<uniform> params: AgentUniforms;

const PI: f32 = 3.14159265;

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn hash_float(seed: u32) -> f32 {
    return f32(wang_hash(seed)) / 4294967296.0;
}

fn sense(pos: vec2<f32>, angle: f32, offset: f32) -> f32 {
    let dir = vec2<f32>(cos(angle + offset), sin(angle + offset));
    let sample_pos = pos + dir * params.sensor_dist;
    let tx = i32(fract(sample_pos.x + 1.0) * f32(params.width)) % i32(params.width);
    let ty = i32(fract(sample_pos.y + 1.0) * f32(params.height)) % i32(params.height);
    return textureLoad(trail_tex, vec2<i32>(tx, ty), 0).r;
}

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.agent_count {
        return;
    }

    var agent = agents[id.x];

    // Beat reactivity
    let beat_frac = fract(params.beat);
    let energy = pow(1.0 - beat_frac, 4.0);
    let deposit_boost = 1.0 + energy * params.reactivity * 2.0;
    let step_boost = 1.0 + energy * params.reactivity * 0.3;

    // Sense in 3 directions
    let sense_f = sense(agent.pos, agent.angle, 0.0);
    let sense_l = sense(agent.pos, agent.angle, -params.sensor_angle);
    let sense_r = sense(agent.pos, agent.angle, params.sensor_angle);

    // Steer toward highest trail
    let rng_seed = wang_hash(id.x * 1299721u + params.frame_count * 6291469u);
    let rand_val = hash_float(rng_seed);

    if sense_f >= sense_l && sense_f >= sense_r {
        // Forward is highest — no turn
    } else if sense_l > sense_r {
        agent.angle -= params.rotation_angle;
    } else if sense_r > sense_l {
        agent.angle += params.rotation_angle;
    } else {
        // Tied left/right — random turn
        if rand_val < 0.5 {
            agent.angle -= params.rotation_angle;
        } else {
            agent.angle += params.rotation_angle;
        }
    }

    // Move
    let step = params.step_size * step_boost;
    agent.pos += vec2<f32>(cos(agent.angle), sin(agent.angle)) * step;
    agent.pos = fract(agent.pos + vec2<f32>(1.0));

    // Deposit into atomic accumulator
    let px = u32(agent.pos.x * f32(params.width)) % params.width;
    let py = u32(agent.pos.y * f32(params.height)) % params.height;
    let idx = py * params.width + px;
    let deposit_val = u32(params.deposit_scaled * deposit_boost);
    atomicAdd(&accum[idx], deposit_val);

    // Write back
    agents[id.x] = agent;
}
