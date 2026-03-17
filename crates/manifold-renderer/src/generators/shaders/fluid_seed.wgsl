// FluidPatternSeed — port of Unity FluidPatternSeed.compute
// GPU-side pattern seeding for FluidSimulationGen snap mode.
// 8 geometric patterns selectable by pattern_index uniform.
// Dispatched once on init or on snap trigger.

struct SeedUniforms {
    active_count: u32,
    pattern_index: u32,
    trigger_count: u32,
    _pad0: u32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(1) var<uniform> params: SeedUniforms;

const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318530718;
const GOLDEN_ANGLE: f32 = 2.39996323; // pi * (3 - sqrt(5))

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

// Triangle-distributed random in ~[-1, 1] (sum of two uniforms - 1)
// Unity: TriRand(seed) = HashFloat(seed) + HashFloat(WangHash(seed)) - 1.0
fn tri_rand(seed: u32) -> f32 {
    return hash_float(seed) + hash_float(wang_hash(seed)) - 1.0;
}

// Returns petal count for rose curve pattern, cycling per trigger group
// Unity: GetPetalK(triggerCount) -> cycles [3,5,7,4,6] per 8 triggers
fn get_petal_k(trigger_count: u32) -> u32 {
    let idx = (trigger_count / 8u) % 5u;
    switch idx {
        case 0u: { return 3u; }
        case 1u: { return 5u; }
        case 2u: { return 7u; }
        case 3u: { return 4u; }
        default: { return 6u; }
    }
}

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= params.active_count {
        return;
    }

    let t = f32(i) / f32(params.active_count);
    let seed = i * 1664525u + params.trigger_count * 7919u;

    var px = 0.5;
    var py = 0.5;

    if params.pattern_index == 0u {
        // Center cluster: triangle-distributed tight cluster at center
        // Unity: px = 0.5 + TriRand(seed) * 0.03
        px = 0.5 + tri_rand(seed) * 0.03;
        py = 0.5 + tri_rand(seed + 1u) * 0.03;
    } else if params.pattern_index == 1u {
        // Horizontal lines (6 lines)
        // Unity: lineIndex = i % 6; px = HashFloat(seed); py = (lineIndex + 0.5) / 6.0
        let line_index = i % 6u;
        px = hash_float(seed);
        py = (f32(line_index) + 0.5) / 6.0;
    } else if params.pattern_index == 2u {
        // Concentric rings (3 rings)
        // Unity: ring = i % 3; radius = 0.1 + ring * 0.1
        let ring = i % 3u;
        let angle = hash_float(seed) * TAU;
        let radius = 0.1 + f32(ring) * 0.1;
        px = 0.5 + cos(angle) * radius;
        py = 0.5 + sin(angle) * radius;
    } else if params.pattern_index == 3u {
        // Grid clusters — 40×40 tight clusters
        // Unity: cell = i % 1600; col = cell % 40; row = cell / 40
        let cell = i % 1600u;
        let col = cell % 40u;
        let row = cell / 40u;
        px = (f32(col) + 0.5) / 40.0 + tri_rand(seed) * 0.005;
        py = (f32(row) + 0.5) / 40.0 + tri_rand(seed + 1u) * 0.005;
    } else if params.pattern_index == 4u {
        // Phyllotaxis — golden angle spiral with density ripples
        // Unity: a = i * GOLDEN_ANGLE; baseR = sqrt(t) * 0.46; r = baseR * (0.6 + 0.4 * sin(baseR * 50))
        let a = f32(i) * GOLDEN_ANGLE;
        let base_r = sqrt(t) * 0.46;
        let r = base_r * (0.6 + 0.4 * sin(base_r * 50.0));
        px = 0.5 + cos(a) * r;
        py = 0.5 + sin(a) * r;
    } else if params.pattern_index == 5u {
        // Rose curve — flower petals, petal count cycles per trigger
        // Unity: k = GetPetalK; theta = t * TWO_PI * k; r = abs(cos(k * theta)) * 0.44
        let k = get_petal_k(params.trigger_count);
        let theta = t * TAU * f32(k);
        let r = abs(cos(f32(k) * theta)) * 0.44;
        let jitter = tri_rand(seed) * 0.003;
        px = 0.5 + cos(theta) * r + jitter;
        py = 0.5 + sin(theta) * r + jitter;
    } else if params.pattern_index == 6u {
        // Spiral galaxy — 5 arms
        // Unity: arm = i % 5; armBase = arm * (TWO_PI/5); theta = t * 4*PI; r = 0.02 + t*0.44
        let arm = i % 5u;
        let arm_base = f32(arm) * (TAU / 5.0);
        let theta = t * 4.0 * PI;
        let r = 0.02 + t * 0.44;
        let a = arm_base + theta * 0.4;
        let jitter = tri_rand(seed) * 0.004;
        px = 0.5 + cos(a) * r + jitter;
        py = 0.5 + sin(a) * r + jitter;
    } else {
        // 7: Vortex seeds — 7 clusters in hex ring (cluster 0 at center, cr=0)
        // Unity: cluster = i % 7; cr = cluster == 0 ? 0.0 : 0.28
        let cluster = i % 7u;
        let cluster_angle = f32(cluster) * (TAU / 7.0);
        let cr = select(0.28, 0.0, cluster == 0u);
        let cx = 0.5 + cos(cluster_angle) * cr;
        let cy = 0.5 + sin(cluster_angle) * cr;
        px = cx + tri_rand(seed) * 0.025;
        py = cy + tri_rand(seed + 1u) * 0.025;
    }

    // Toroidal wrap (Unity: frac(px + 1.0))
    var pos = fract(vec2<f32>(px, py) + vec2<f32>(1.0));

    var p: Particle;
    p.position = vec3<f32>(pos, 0.0);
    p.velocity = vec3<f32>(0.0);
    p.life = 1.0;
    p.age = 0.0;
    p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);

    particles[i] = p;
}
