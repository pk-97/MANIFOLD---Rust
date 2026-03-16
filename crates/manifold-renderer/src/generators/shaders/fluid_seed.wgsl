// Particle seed patterns for fluid simulation.
// 7 geometric patterns selectable by pattern_index uniform.
// Dispatched once on init or on snap trigger.

struct SeedUniforms {
    active_count: u32,
    pattern_index: u32,
    _pad0: u32,
    _pad1: u32,
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
const TAU: f32 = 6.28318530;

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

// Box-Muller approximation for Gaussian distribution
fn gaussian_pair(seed: u32) -> vec2<f32> {
    let u1 = max(hash_float(seed), 0.0001);
    let u2 = hash_float(wang_hash(seed));
    let r = sqrt(-2.0 * log(u1));
    return vec2<f32>(r * cos(TAU * u2), r * sin(TAU * u2));
}

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.active_count {
        return;
    }

    let rng_base = wang_hash(id.x * 2654435761u + 12345u);

    var pos = vec2<f32>(0.5);
    let pattern = params.pattern_index;

    if pattern == 0u {
        // Center cluster: Gaussian distribution around (0.5, 0.5)
        let g = gaussian_pair(rng_base);
        pos = vec2<f32>(0.5 + g.x * 0.15, 0.5 + g.y * 0.15);
    } else if pattern == 1u {
        // Horizontal lines: 3 bands
        let band = id.x % 3u;
        let t = hash_float(rng_base);
        pos.x = t;
        pos.y = (f32(band) + 0.5) / 3.0 + (hash_float(wang_hash(rng_base)) - 0.5) * 0.02;
    } else if pattern == 2u {
        // Vertical lines: 3 bands
        let band = id.x % 3u;
        let t = hash_float(rng_base);
        pos.y = t;
        pos.x = (f32(band) + 0.5) / 3.0 + (hash_float(wang_hash(rng_base)) - 0.5) * 0.02;
    } else if pattern == 3u {
        // Concentric rings: 3 rings from center
        let ring = id.x % 3u;
        let angle = hash_float(rng_base) * TAU;
        let radius = (f32(ring) + 1.0) * 0.12 + (hash_float(wang_hash(rng_base)) - 0.5) * 0.01;
        pos = vec2<f32>(0.5 + cos(angle) * radius, 0.5 + sin(angle) * radius);
    } else if pattern == 4u {
        // Diagonal cross (X pattern)
        let arm = id.x % 2u;
        let t = hash_float(rng_base);
        let spread = (hash_float(wang_hash(rng_base)) - 0.5) * 0.03;
        if arm == 0u {
            pos = vec2<f32>(t, t + spread);
        } else {
            pos = vec2<f32>(t, 1.0 - t + spread);
        }
    } else if pattern == 5u {
        // Spiral: Archimedean spiral from center
        let t = hash_float(rng_base);
        let angle = t * TAU * 3.0;
        let radius = t * 0.4;
        let jitter = (hash_float(wang_hash(rng_base)) - 0.5) * 0.015;
        pos = vec2<f32>(
            0.5 + (radius + jitter) * cos(angle),
            0.5 + (radius + jitter) * sin(angle),
        );
    } else {
        // Edge ring: particles on border
        let t = hash_float(rng_base);
        let edge = id.x % 4u;
        let jitter = (hash_float(wang_hash(rng_base)) - 0.5) * 0.02;
        if edge == 0u {
            pos = vec2<f32>(t, jitter);
        } else if edge == 1u {
            pos = vec2<f32>(t, 1.0 + jitter);
        } else if edge == 2u {
            pos = vec2<f32>(jitter, t);
        } else {
            pos = vec2<f32>(1.0 + jitter, t);
        }
    }

    // Toroidal wrap
    pos = fract(pos + vec2<f32>(1.0));

    var p: Particle;
    p.position = vec3<f32>(pos, 0.0);
    p.velocity = vec3<f32>(0.0);
    p.life = 0.5 + hash_float(wang_hash(wang_hash(rng_base))) * 0.5;
    p.age = 0.0;
    p.color = vec4<f32>(1.0);

    particles[id.x] = p;
}
