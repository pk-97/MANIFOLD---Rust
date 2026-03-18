// VoronoiPrism effect — per-cell UV remapping with beat-synchronized pop-in.
// Unity ref: VoronoiPrismEffect.shader

struct Uniforms {
    amount: f32,
    cell_count: f32,
    beat: f32,
    aspect_ratio: f32,
    source_width: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// Hash function for pseudo-random cell points.
// Unity ref: VoronoiPrismEffect.shader hash2()
fn hash2(p: vec2<f32>) -> vec2<f32> {
    let q = vec2<f32>(
        dot(p, vec2<f32>(127.1, 311.7)),
        dot(p, vec2<f32>(269.5, 183.3)),
    );
    return fract(sin(q) * 43758.5453);
}

// Single-value hash.
// Unity ref: VoronoiPrismEffect.shader hash1()
fn hash1(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(41.7, 289.3))) * 18743.291);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(source_tex, tex_sampler, in.uv);
    let original = src.rgb;

    // Beat-derived values: changes every beat
    let beat_floor = floor(uniforms.beat);
    let beat_frac = fract(uniforms.beat);

    // Scale UV to cell grid (aspect-corrected so cells are square)
    let scaled_uv = in.uv * vec2<f32>(uniforms.cell_count * uniforms.aspect_ratio, uniforms.cell_count);
    let cell_id = floor(scaled_uv);
    let cell_uv = fract(scaled_uv);

    // Find nearest and second-nearest Voronoi points
    var min_dist: f32 = 10.0;
    var second_dist: f32 = 10.0;
    var nearest_cell_id: vec2<f32> = vec2<f32>(0.0, 0.0);

    for (var dy: i32 = -1; dy <= 1; dy++) {
        for (var dx: i32 = -1; dx <= 1; dx++) {
            let neighbor = vec2<f32>(f32(dx), f32(dy));
            let pt = hash2(cell_id + neighbor);
            let diff = neighbor + pt - cell_uv;
            let dist = dot(diff, diff);
            if (dist < min_dist) {
                second_dist = min_dist;
                min_dist = dist;
                nearest_cell_id = cell_id + neighbor;
            } else if (dist < second_dist) {
                second_dist = dist;
            }
        }
    }

    // Per-cell beat hash: each cell gets a unique random value per beat
    let cell_beat_hash = hash1(nearest_cell_id + vec2<f32>(beat_floor * 0.17, beat_floor * 0.31));

    // Cell on/off: ~40% of cells go dark each beat
    let cell_active = step(0.4, cell_beat_hash);

    // Smooth pop-in on beat transition (quick 15% of beat)
    let pop_in = clamp(beat_frac / 0.15, 0.0, 1.0);

    // Content band boundaries (where the actual video sits)
    let half_w = uniforms.source_width * 0.5;
    let content_left = 0.5 - half_w;
    let content_right = 0.5 + half_w;

    // Remap base UV.x into content region (pixels in black bars -> nearest content edge)
    let base_x = clamp(in.uv.x, content_left, content_right);

    // Each cell offsets the UV — offset shifts each beat for reordering
    let beat_seed = vec2<f32>(beat_floor * 1.73, beat_floor * 2.91);
    let cell_hash = hash2(nearest_cell_id + beat_seed);
    let uv_offset = (cell_hash - 0.5) * 0.4;
    var prism_uv = vec2<f32>(base_x, in.uv.y) + uv_offset;
    // Clamp to content band so no cell ever samples black bars
    prism_uv.x = clamp(prism_uv.x, content_left, content_right);
    prism_uv.y = clamp(prism_uv.y, 0.0, 1.0);

    let prism_sample = textureSample(source_tex, tex_sampler, prism_uv);
    var prism = prism_sample.rgb;

    // Visibility: hard on/off per beat (inactive cells go black)
    let visibility = cell_active * pop_in;
    prism = prism * visibility;

    let result = mix(original, prism, uniforms.amount);
    return vec4<f32>(result, mix(src.a, prism_sample.a * visibility, uniforms.amount));
}
