// node.temporal_accumulate — reprojected temporal accumulation (Jimenez et
// al. 2016, GTAO section 5: the noise moves every frame and the history
// averages it). Last frame's result is fetched where the render's motion
// vectors say this pixel was, each of the four bilinear history taps is kept
// only where its stored depth matches the depth this surface had last frame,
// the kept history is clipped to the current neighbourhood (Salvi 2016
// variance clipping), then blended with a 1/n running mean capped at
// `frames`.

struct Uniforms {
    // Last frame's view depth of this frame's view-space point v:
    // dot(prev_depth.xyz, v) + prev_depth.w.
    prev_depth: vec4<f32>,
    tan_half_fov: f32,
    aspect: f32,
    near: f32,
    far: f32,
    frames: f32,
    // 1 when there is no usable history (first frame, cut, seek, resize).
    cold: u32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var in_tex: texture_2d<f32>;
@group(0) @binding(2) var depth_tex: texture_2d<f32>;
@group(0) @binding(3) var velocity_tex: texture_2d<f32>;
// History colour, and (view depth, frames accumulated) per pixel.
@group(0) @binding(4) var history_color: texture_2d<f32>;
@group(0) @binding(5) var history_meta: texture_2d<f32>;
@group(0) @binding(6) var output_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(7) var next_color: texture_storage_2d<rgba16float, write>;
@group(0) @binding(8) var next_meta: texture_storage_2d<rgba16float, write>;

// History taps whose depth differs by more than this fraction, beyond the
// local depth slope, belong to another surface.
const DEPTH_TOLERANCE: f32 = 0.02;
// Variance-clip width in standard deviations.
const CLIP_GAMMA: f32 = 1.25;

fn texel_at(uv: vec2<f32>, dims: vec2<u32>) -> vec2<i32> {
    let p = vec2<i32>(floor(uv * vec2<f32>(dims)));
    return clamp(p, vec2<i32>(0), vec2<i32>(dims) - 1);
}

fn view_depth_at(uv: vec2<f32>) -> f32 {
    let raw = textureLoad(depth_tex, texel_at(uv, textureDimensions(depth_tex)), 0).r;
    return linearize_depth(raw, u.near, u.far);
}

@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let p = vec2<i32>(id.xy);
    let texel = 1.0 / vec2<f32>(dims);
    let uv = (vec2<f32>(id.xy) + 0.5) * texel;
    let current = textureLoad(in_tex, p, 0);
    let z = view_depth_at(uv);

    var result = current;
    var n = 1.0;
    let vel = textureLoad(velocity_tex, texel_at(uv, textureDimensions(velocity_tex)), 0).xy;
    // Velocity is NDC (now - previous); NDC y points up, uv y down.
    let prev_uv = uv + vec2<f32>(-vel.x, vel.y) * 0.5;
    let on_screen = all(prev_uv >= vec2<f32>(0.0)) && all(prev_uv <= vec2<f32>(1.0));
    if u.cold == 0u && on_screen {
        let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
        let v = vec3<f32>(ndc.x * u.tan_half_fov * u.aspect * z, ndc.y * u.tan_half_fov * z, z);
        let expected = dot(u.prev_depth.xyz, v) + u.prev_depth.w;
        // The steeper side of a slope is another surface at a silhouette, so
        // each axis takes its gentler side.
        let slope_x = min(abs(view_depth_at(uv + vec2<f32>(texel.x, 0.0)) - z), abs(view_depth_at(uv - vec2<f32>(texel.x, 0.0)) - z));
        let slope_y = min(abs(view_depth_at(uv + vec2<f32>(0.0, texel.y)) - z), abs(view_depth_at(uv - vec2<f32>(0.0, texel.y)) - z));
        let tolerance = DEPTH_TOLERANCE * expected + slope_x + slope_y;

        let hist_dims = vec2<i32>(textureDimensions(history_color));
        let f = prev_uv * vec2<f32>(hist_dims) - 0.5;
        let base = vec2<i32>(floor(f));
        let t = f - floor(f);
        var color_sum = vec4<f32>(0.0);
        var n_sum = 0.0;
        var w_sum = 0.0;
        for (var k = 0; k < 4; k = k + 1) {
            let o = vec2<i32>(k & 1, k >> 1);
            let q = base + o;
            if any(q < vec2<i32>(0)) || any(q >= hist_dims) {
                continue;
            }
            let stored = textureLoad(history_meta, q, 0);
            if stored.y < 1.0 || abs(stored.x - expected) > tolerance {
                continue;
            }
            let wx = select(1.0 - t.x, t.x, o.x == 1);
            let wy = select(1.0 - t.y, t.y, o.y == 1);
            let w = wx * wy;
            color_sum = color_sum + textureLoad(history_color, q, 0) * w;
            n_sum = n_sum + stored.y * w;
            w_sum = w_sum + w;
        }
        if w_sum > 1e-3 {
            var m1 = vec4<f32>(0.0);
            var m2 = vec4<f32>(0.0);
            for (var dy = -1; dy <= 1; dy = dy + 1) {
                for (var dx = -1; dx <= 1; dx = dx + 1) {
                    let s = textureLoad(in_tex, clamp(p + vec2<i32>(dx, dy), vec2<i32>(0), vec2<i32>(dims) - 1), 0);
                    m1 = m1 + s;
                    m2 = m2 + s * s;
                }
            }
            m1 = m1 / 9.0;
            let sigma = sqrt(max(m2 / 9.0 - m1 * m1, vec4<f32>(0.0)));
            let history = clamp(color_sum / w_sum, m1 - CLIP_GAMMA * sigma, m1 + CLIP_GAMMA * sigma);
            n = min(n_sum / w_sum + 1.0, max(u.frames, 1.0));
            result = mix(history, current, 1.0 / n);
        }
    }
    textureStore(output_tex, p, result);
    textureStore(next_color, p, result);
    textureStore(next_meta, p, vec4<f32>(z, n, 0.0, 0.0));
}
