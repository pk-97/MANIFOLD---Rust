// Temporal Anti-Aliasing — exponential history blend.
//
// Each frame is rendered with a sub-pixel jitter offset. This shader blends
// the current jittered frame with an exponentially-weighted history buffer,
// accumulating real sub-pixel detail over ~8 frames.
//
// No motion vectors — simple blend with configurable weight.
// Fast-moving content naturally converges because new frames dominate.

struct TaaUniforms {
    // Blend weight for the current frame. Higher = less ghosting, less AA.
    // 0.1 = strong accumulation (smooth but ghosty on motion)
    // 0.2 = balanced (good AA, mild trailing on fast motion)
    // 0.3 = responsive (less AA, minimal ghosting)
    blend_weight: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> params: TaaUniforms;
@group(0) @binding(1) var t_current: texture_2d<f32>;
@group(0) @binding(2) var t_history: texture_2d<f32>;
@group(0) @binding(3) var t_output: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(t_output);
    if id.x >= dims.x || id.y >= dims.y { return; }

    let coord = vec2<i32>(id.xy);
    let current = textureLoad(t_current, coord, 0);
    let history = textureLoad(t_history, coord, 0);

    // Neighbourhood clamp: restrict history to the local min/max of the
    // current frame's 3×3 neighbourhood. Prevents ghosting on edges that
    // moved since the previous frame.
    let tl = textureLoad(t_current, coord + vec2<i32>(-1, -1), 0).rgb;
    let tc = textureLoad(t_current, coord + vec2<i32>( 0, -1), 0).rgb;
    let tr = textureLoad(t_current, coord + vec2<i32>( 1, -1), 0).rgb;
    let ml = textureLoad(t_current, coord + vec2<i32>(-1,  0), 0).rgb;
    let mr = textureLoad(t_current, coord + vec2<i32>( 1,  0), 0).rgb;
    let bl = textureLoad(t_current, coord + vec2<i32>(-1,  1), 0).rgb;
    let bc = textureLoad(t_current, coord + vec2<i32>( 0,  1), 0).rgb;
    let br = textureLoad(t_current, coord + vec2<i32>( 1,  1), 0).rgb;

    let nmin = min(min(min(tl, tc), min(tr, ml)), min(min(mr, bl), min(bc, br)));
    let nmin3 = min(nmin, current.rgb);
    let nmax = max(max(max(tl, tc), max(tr, ml)), max(max(mr, bl), max(bc, br)));
    let nmax3 = max(nmax, current.rgb);

    let clamped_history = vec4<f32>(clamp(history.rgb, nmin3, nmax3), history.a);

    // Exponential blend: result = current * w + history * (1 - w)
    let result = mix(clamped_history, current, params.blend_weight);

    textureStore(t_output, coord, result);
}
