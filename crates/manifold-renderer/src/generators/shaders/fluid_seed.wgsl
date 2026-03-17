// FluidPatternSeed — port of Unity FluidParticleSimulate.compute SeedPatternKernel
// GPU-side pattern seeding for FluidSimulationGen snap mode.
// 7 geometric patterns (0-6) selectable by pattern_index uniform.
// Dispatched once on init or on snap trigger.
//
// Unity patterns:
//   0: Center cluster (CLT Gaussian approximation)
//   1: Horizontal lines (6 lines)
//   2: Vertical lines (6 lines)
//   3: Concentric rings (3 rings)
//   4: Diagonal cross
//   5: Spiral (Archimedean)
//   6: Edge ring

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

const TAU: f32 = 6.28318530718;

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

fn hash_float2(seed: u32) -> vec2<f32> {
    let h1 = wang_hash(seed);
    let h2 = wang_hash(h1);
    return vec2<f32>(f32(h1), f32(h2)) / 4294967296.0;
}

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= params.active_count {
        return;
    }

    // Unity: uint seed = i * 1664525u + _PatternSeed * 747796405u;
    let seed = i * 1664525u + params.trigger_count * 747796405u;

    var x: f32;
    var y: f32;

    if params.pattern_index == 0u {
        // Case 0: Center cluster (CLT Gaussian approximation)
        // Unity: sum 4 HashFloat2 values, spread 0.052
        var sx = 0.0;
        var sy = 0.0;
        var s = seed;
        for (var j = 0; j < 4; j++) {
            let h = hash_float2(s);
            sx += h.x;
            sy += h.y;
            s = wang_hash(s);
        }
        x = 0.5 + (sx - 2.0) * 0.052;
        y = 0.5 + (sy - 2.0) * 0.052;
    } else if params.pattern_index == 1u {
        // Case 1: Horizontal lines (6 lines)
        // Unity: lineIdx = i % 6u; x = HashFloat(seed); y = (lineIdx + 0.5) / 6.0
        let line_idx = i % 6u;
        x = hash_float(seed);
        y = (f32(line_idx) + 0.5) / 6.0;
    } else if params.pattern_index == 2u {
        // Case 2: Vertical lines (6 lines)
        // Unity: lineIdx = i % 6u; x = (lineIdx + 0.5) / 6.0; y = HashFloat(seed)
        let line_idx = i % 6u;
        x = (f32(line_idx) + 0.5) / 6.0;
        y = hash_float(seed);
    } else if params.pattern_index == 3u {
        // Case 3: Concentric rings (3 rings)
        // Unity: ringIdx = i % 3u; radius = 0.1 + ringIdx * 0.1
        let ring_idx = i % 3u;
        let angle = hash_float(seed) * TAU;
        let radius = 0.1 + f32(ring_idx) * 0.1;
        x = 0.5 + cos(angle) * radius;
        y = 0.5 + sin(angle) * radius;
    } else if params.pattern_index == 4u {
        // Case 4: Diagonal cross — two bands forming an X
        // Unity: t = HashFloat(seed); spread = HashFloat(seed+1) * 0.03
        let t = hash_float(seed);
        let spread = hash_float(seed + 1u) * 0.03;
        if (i & 1u) != 0u {
            x = t + spread;
            y = t - spread;
        } else {
            x = t + spread;
            y = (1.0 - t) - spread;
        }
    } else if params.pattern_index == 5u {
        // Case 5: Spiral — Archimedean spiral from center
        // Unity: norm = i / ParticleCount; angle = norm * 4 * TWO_PI; radius = norm * 0.4
        let norm = f32(i) / f32(params.active_count);
        let angle = norm * 4.0 * TAU;
        let radius = norm * 0.4;
        let spread = hash_float(seed) * 0.015;
        x = 0.5 + cos(angle) * (radius + spread);
        y = 0.5 + sin(angle) * (radius + spread);
    } else if params.pattern_index == 6u {
        // Case 6: Edge ring — ring near boundary, implodes inward
        // Unity: angle = HashFloat(seed) * TWO_PI; spread = (HashFloat(seed+1) - 0.5) * 0.03
        let angle = hash_float(seed) * TAU;
        let spread = (hash_float(seed + 1u) - 0.5) * 0.03;
        x = 0.5 + cos(angle) * (0.45 + spread);
        y = 0.5 + sin(angle) * (0.45 + spread);
    } else {
        // Default: random position
        let pos = hash_float2(seed);
        x = pos.x;
        y = pos.y;
    }

    // Unity: p.position = float3(frac(x + 1.0), frac(y + 1.0), 0.0)
    var p: Particle;
    p.position = vec3<f32>(fract(x + 1.0), fract(y + 1.0), 0.0);
    p.velocity = vec3<f32>(0.0);
    p.life = 1.0;
    // Unity: p.age = -1.0 (uncolored marker)
    p.age = -1.0;
    p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);

    particles[i] = p;
}
