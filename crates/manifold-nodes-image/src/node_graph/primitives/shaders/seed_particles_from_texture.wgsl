// node.seed_particles_from_texture — exact-placement particle seeding
// from a Texture2D density mask.
//
// DETERMINISTIC stream compaction (four-pass dispatch). The list of bright
// texels must come out in a canonical, race-free order — particle i maps to
// bright_list[i mod count], and everything downstream that is keyed by the
// particle index (the sub-texel jitter here, node.anti_clump_particles'
// hash(i, frame) kick) would otherwise be paired with a DIFFERENT texel every
// run. In a chaotic feedback sim that scrambles the trajectories and the same
// clip renders differently each trigger. A single global `atomicAdd` slot is
// arrival-order, i.e. non-deterministic, so we compact via an explicit prefix
// sum over fixed 256-texel blocks instead — the block bases are summed in scan
// order, so bright_list ends up in exact row-major texel order every time.
//
//   count_main   : per 256-texel block, count its bright texels (R > 0.1).
//                  No atomics — one thread per block walks its own block.
//   scan_main    : single thread, exclusive prefix sum of the block counts
//                  in place → each block's base offset; total → counter.
//   compact_main : per block, walk its texels in scan order and append each
//                  bright UV at the block's base offset, incrementing locally.
//   place_main   : for each particle i, assign bright_list[i mod count] with a
//                  small sub-texel hash-jitter so multiple particles per bright
//                  pixel don't visually stack.
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
// Per-block scratch: bright-texel count per 256-texel block (count_main),
// rewritten in place to each block's exclusive base offset (scan_main).
@group(0) @binding(5) var<storage, read_write> block_data: array<u32>;

const THRESHOLD: f32 = 0.1;
const BLOCK: u32 = 256u;

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

// Bright test for the linear texel index `lin` (row-major over the mask).
fn is_bright(lin: u32) -> bool {
    let x = lin % params.tex_width;
    let y = lin / params.tex_width;
    let v = textureLoad(mask, vec2<i32>(i32(x), i32(y)), 0).r;
    return v > THRESHOLD;
}

fn num_blocks() -> u32 {
    let total = params.tex_width * params.tex_height;
    return (total + BLOCK - 1u) / BLOCK;
}

// Pass 1 — count bright texels in each 256-texel block. One thread per block,
// no atomics: a direct write to block_data[block], deterministic by construction.
@compute @workgroup_size(64)
fn count_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let block = id.x;
    if block >= num_blocks() {
        return;
    }
    let total = params.tex_width * params.tex_height;
    let start = block * BLOCK;
    let end = min(start + BLOCK, total);
    var c = 0u;
    for (var lin = start; lin < end; lin = lin + 1u) {
        if is_bright(lin) {
            c = c + 1u;
        }
    }
    block_data[block] = c;
}

// Pass 2 — exclusive prefix sum of the block counts, single thread. Rewrites
// block_data[b] from "count of block b" to "base offset of block b", and
// stores the grand total into counter for place_main. Sequential, so the
// in-place read-then-overwrite is safe.
@compute @workgroup_size(1)
fn scan_main() {
    let nblocks = num_blocks();
    var acc = 0u;
    for (var b = 0u; b < nblocks; b = b + 1u) {
        let c = block_data[b];
        block_data[b] = acc;
        acc = acc + c;
    }
    atomicStore(&counter, acc);
}

// Pass 3 — compact. One thread per block: walk the block's texels in scan
// order and append each bright UV starting at the block's base offset. Because
// the bases were summed in block order and each block writes its texels in
// linear order, bright_list comes out in exact row-major order every run.
@compute @workgroup_size(64)
fn compact_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let block = id.x;
    if block >= num_blocks() {
        return;
    }
    let total = params.tex_width * params.tex_height;
    let start = block * BLOCK;
    let end = min(start + BLOCK, total);
    var slot = block_data[block];
    for (var lin = start; lin < end; lin = lin + 1u) {
        if is_bright(lin) {
            if slot < params.list_capacity {
                let x = lin % params.tex_width;
                let y = lin / params.tex_width;
                let uv = (vec2<f32>(f32(x), f32(y)) + vec2<f32>(0.5))
                    / vec2<f32>(f32(params.tex_width), f32(params.tex_height));
                bright_list[slot] = uv;
            }
            slot = slot + 1u;
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
