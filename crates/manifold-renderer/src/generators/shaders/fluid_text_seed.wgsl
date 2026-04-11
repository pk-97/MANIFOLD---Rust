// FluidTextSeed — seed particles at positions sampled from an R8 text bitmap.
//
// Each thread handles one particle. Bright pixels in the text texture are
// candidate spawn positions. Particles are distributed across the text shape
// using a hash-based random UV, rejection-sampled against the bitmap.
// Particles that fail rejection get random positions (ensures all particles
// are placed).

struct TextSeedUniforms {
    active_count: u32,
    tex_width: u32,
    tex_height: u32,
    frame_seed: u32,
    // Text placement: center UV and scale
    center_x: f32,
    center_y: f32,
    text_scale: f32,
    aspect_ratio: f32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<storage, read_write> particles: array<Particle>;
@group(0) @binding(1) var<uniform> params: TextSeedUniforms;
@group(0) @binding(2) var text_tex: texture_2d<f32>;

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

@compute @workgroup_size(256, 1, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= params.active_count {
        return;
    }

    let seed = i * 1664525u + params.frame_seed * 747796405u;
    let tw = params.tex_width;
    let th = params.tex_height;

    // Aspect ratio of the text bitmap
    let text_aspect = f32(tw) / f32(th);

    // Text occupies a region in UV space centered at (center_x, center_y).
    // Scale determines the height of the text region in UV units.
    let region_h = params.text_scale;
    let region_w = region_h * text_aspect / params.aspect_ratio;

    var x: f32;
    var y: f32;
    var placed = false;

    // Rejection sampling: try up to 16 random positions within the text region.
    // Accept if the corresponding texel is bright (> 0.1).
    var s = seed;
    for (var attempt = 0; attempt < 16; attempt++) {
        let rx = hash_float(s);
        s = wang_hash(s);
        let ry = hash_float(s);
        s = wang_hash(s);

        // Map to texel coordinates
        let tx = u32(rx * f32(tw));
        let ty = u32(ry * f32(th));
        let texel = textureLoad(text_tex, vec2<u32>(tx, ty), 0).r;

        if texel > 0.1 {
            // Map texel position to UV space
            x = params.center_x + (rx - 0.5) * region_w;
            y = params.center_y + (ry - 0.5) * region_h;
            placed = true;
            break;
        }
    }

    if !placed {
        // Fallback: random position across full canvas
        x = hash_float(s);
        s = wang_hash(s);
        y = hash_float(s);
    }

    var p: Particle;
    p.position = vec3<f32>(fract(x + 1.0), fract(y + 1.0), 0.0);
    p.velocity = vec3<f32>(0.0);
    p.life = 1.0;
    p.age = -1.0; // uncolored marker
    p.color = vec4<f32>(0.005, 0.005, 0.005, 1.0);

    particles[i] = p;
}
