// node.simplex_noise_2d — 2D Simplex noise.
//
// Classic Ashima Arts 2D simplex (Stefan Gustavson / Ian McEwan).
// Output is in the canonical [-1, 1] range remapped to [0, 1]
// for storage convenience: 0.5 * (raw + 1.0). Broadcast to RGB,
// A = 1.
//
// Inputs: scale (frequency multiplier in UV-units), offset (XY pan).
// To animate, drive offset from an LFO upstream.

struct Uniforms {
    scale:    f32,   // higher = finer detail. 1.0 ≈ one cell across the texture.
    offset_x: f32,
    offset_y: f32,
    _pad:     f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

// ---------------- Ashima 2D simplex ----------------

fn mod289_v3(x: vec3<f32>) -> vec3<f32> {
    return x - floor(x * (1.0 / 289.0)) * 289.0;
}

fn mod289_v2(x: vec2<f32>) -> vec2<f32> {
    return x - floor(x * (1.0 / 289.0)) * 289.0;
}

fn permute_v3(x: vec3<f32>) -> vec3<f32> {
    return mod289_v3(((x * 34.0) + 1.0) * x);
}

fn snoise_2d(v_in: vec2<f32>) -> f32 {
    let C = vec4<f32>(
        0.211324865405187,   // (3.0 - sqrt(3.0)) / 6.0
        0.366025403784439,   // 0.5 * (sqrt(3.0) - 1.0)
       -0.577350269189626,   // -1.0 + 2.0 * C.x
        0.024390243902439    // 1.0 / 41.0
    );

    var v = v_in;
    var i  = floor(v + dot(v, C.yy));
    let x0 = v - i + dot(i, C.xx);

    var i1: vec2<f32>;
    if x0.x > x0.y {
        i1 = vec2<f32>(1.0, 0.0);
    } else {
        i1 = vec2<f32>(0.0, 1.0);
    }

    var x12 = x0.xyxy + C.xxzz;
    x12 = vec4<f32>(x12.xy - i1, x12.zw);

    i = mod289_v2(i);
    let p = permute_v3(
        permute_v3(i.y + vec3<f32>(0.0, i1.y, 1.0))
        + i.x + vec3<f32>(0.0, i1.x, 1.0)
    );

    var m = max(
        0.5 - vec3<f32>(dot(x0, x0), dot(x12.xy, x12.xy), dot(x12.zw, x12.zw)),
        vec3<f32>(0.0)
    );
    m = m * m;
    m = m * m;

    let x  = 2.0 * fract(p * C.www) - 1.0;
    let h  = abs(x) - 0.5;
    let ox = floor(x + 0.5);
    let a0 = x - ox;

    m = m * (1.79284291400159 - 0.85373472095314 * (a0 * a0 + h * h));

    var g: vec3<f32>;
    g.x  = a0.x * x0.x  + h.x * x0.y;
    g.y  = a0.y * x12.x + h.y * x12.y;
    g.z  = a0.z * x12.z + h.z * x12.w;
    return 130.0 * dot(m, g);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let p = uv * u.scale + vec2<f32>(u.offset_x, u.offset_y);
    let n = snoise_2d(p);
    let v = 0.5 * (n + 1.0);   // remap [-1, 1] → [0, 1]
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(v, v, v, 1.0));
}
