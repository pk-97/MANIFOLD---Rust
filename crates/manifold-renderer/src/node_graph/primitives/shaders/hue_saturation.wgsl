// node.hue_saturation — HSV colour adjust. Rotate hue (degrees), scale
// saturation and value, in HSV space. RGB→HSV→adjust→RGB.

struct Uniforms {
    hue_degrees: f32,
    saturation: f32,
    value: f32,
    _pad0: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

// Sam Hocevar's branchless RGB↔HSV.
fn rgb2hsv(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
    let p = mix(vec4<f32>(c.bg, K.wz), vec4<f32>(c.gb, K.xy), step(c.b, c.g));
    let q = mix(vec4<f32>(p.xyw, c.r), vec4<f32>(c.r, p.yzx), step(p.x, c.r));
    let d = q.x - min(q.w, q.y);
    let e = 1.0e-10;
    return vec3<f32>(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}

fn hsv2rgb(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    let p = abs(fract(vec3<f32>(c.x) + K.xyz) * 6.0 - vec3<f32>(K.w));
    return c.z * mix(vec3<f32>(K.x), clamp(p - vec3<f32>(K.x), vec3<f32>(0.0), vec3<f32>(1.0)), c.y);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    var hsv = rgb2hsv(max(src.rgb, vec3<f32>(0.0)));
    hsv.x = fract(hsv.x + uniforms.hue_degrees / 360.0);
    hsv.y = clamp(hsv.y * uniforms.saturation, 0.0, 1.0);
    hsv.z = hsv.z * uniforms.value;

    let rgb = hsv2rgb(hsv);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(rgb, src.a));
}
