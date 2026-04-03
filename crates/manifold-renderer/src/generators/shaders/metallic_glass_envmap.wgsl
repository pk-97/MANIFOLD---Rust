// Metallic Glass — Minimal HDR environment map.
//
// Subtle ambient fill only — prevents pure black reflections on metallic
// surfaces while letting the single point light dominate the look.
// 512×256 equirectangular, generated once at init.

@group(0) @binding(0) var dst_tex: texture_storage_2d<rgba16float, write>;

const PI: f32 = 3.14159265;

fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let width = 512u;
    let height = 256u;
    if gid.x >= width || gid.y >= height { return; }

    let v_coord = f32(gid.y) / f32(height);
    let elevation = v_coord * PI - PI * 0.5;

    // Simple vertical gradient: slightly brighter above, darker below.
    // Mimics ambient sky/ceiling bounce without distinct light sources.
    let up = sin(elevation);
    let ambient = 0.12 + up * 0.08;

    // Subtle noise to break up perfect uniformity
    let noise = hash21(vec2<f32>(f32(gid.x), f32(gid.y))) * 0.02;

    let val = max(ambient + noise, 0.05);
    let color = vec3<f32>(val, val, val * 1.05);  // very slight cool tint

    textureStore(dst_tex, vec2<i32>(gid.xy), vec4<f32>(color, 1.0));
}
