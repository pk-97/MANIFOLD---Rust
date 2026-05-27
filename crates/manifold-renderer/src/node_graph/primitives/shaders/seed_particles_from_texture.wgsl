// node.seed_particles_from_texture — exact-placement particle seeding
// from a Texture2D density mask.
//
// Two-pass dispatch:
//   compact_main : scan the mask, atomically append every bright texel's
//                  UV (R > 0.1) into `bright_list`. Single global counter
//                  tracks the list length.
//   place_main   : for each particle i, assign it `bright_list[i mod
//                  count]` with a small sub-texel hash-jitter so multiple
//                  particles per bright pixel don't visually stack.
//
// Guarantees: every active particle ends up alive on a bright texel (no
// rejection-sampling dead-particle failure modes). If active_count >
// bright_count, particles wrap round-robin and share pixels (jittered).
// If the mask is empty (no bright texels) every particle is parked dead
// at center.
//
// UV mapping matches the legacy fluid_text_seed.wgsl:
//   the mask is centered at (0.5, 0.5) and sized
//   (tex_width / output_width, tex_height / output_height).
//   For full-frame masks, output_width / output_height equal the mask
//   dimensions and the mapping is the identity.

struct Params {
    active_count: u32,
    frame_seed: u32,
    tex_width: u32,
    tex_height: u32,
    output_width: f32,
    output_height: f32,
    list_capacity: u32,
    _pad: u32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var mask: texture_2d<f32>;
@group(0) @binding(3) var<storage, read_write> bright_list: array<vec2<f32>>;
@group(0) @binding(4) var<storage, read_write> counter: atomic<u32>;

const THRESHOLD: f32 = 0.1;

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

@compute @workgroup_size(16, 16)
fn compact_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.tex_width || id.y >= params.tex_height {
        return;
    }
    let v = textureLoad(mask, vec2<i32>(i32(id.x), i32(id.y)), 0).r;
    if v > THRESHOLD {
        let idx = atomicAdd(&counter, 1u);
        if idx < params.list_capacity {
            let uv = (vec2<f32>(f32(id.x), f32(id.y)) + vec2<f32>(0.5))
                / vec2<f32>(f32(params.tex_width), f32(params.tex_height));
            bright_list[idx] = uv;
        }
    }
}

@compute @workgroup_size(256, 1, 1)
fn place_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= params.active_count {
        return;
    }

    let raw_count = atomicLoad(&counter);
    let count = min(raw_count, params.list_capacity);

    var p: Particle;
    p.velocity = vec3<f32>(0.0);
    p.age = -1.0;
    p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);

    if count == 0u {
        // Mask had no bright texels — park dead at center.
        p.position = vec3<f32>(0.5, 0.5, 0.0);
        p.life = 0.0;
    } else {
        // Stable per-particle assignment: particle i maps to bright pixel
        // (i mod count). Wrap-around when active_count > count, so
        // multiple particles can share a pixel — disambiguated by the
        // sub-texel jitter below.
        let bright_idx = i % count;
        let uv = bright_list[bright_idx];

        // Jitter within the source texel so stacked particles don't
        // render to the same display pixel. frame_seed mixes in so the
        // jitter pattern can be perturbed externally.
        let h = wang_hash(i ^ (params.frame_seed * 747796405u));
        let jx = (hash_float(h) - 0.5) / f32(params.tex_width);
        let jy = (hash_float(wang_hash(h)) - 0.5) / f32(params.tex_height);

        // Map mask UV → output (particle) UV. Mask is centered at
        // (0.5, 0.5) and sized (tex/output) of the unit square.
        let region = vec2<f32>(
            f32(params.tex_width) / params.output_width,
            f32(params.tex_height) / params.output_height,
        );
        let jittered = uv + vec2<f32>(jx, jy);
        let pos = vec2<f32>(0.5) + (jittered - vec2<f32>(0.5)) * region;
        p.position = vec3<f32>(fract(pos.x + 1.0), fract(pos.y + 1.0), 0.0);
        p.life = 1.0;
    }

    particles[i] = p;
}
