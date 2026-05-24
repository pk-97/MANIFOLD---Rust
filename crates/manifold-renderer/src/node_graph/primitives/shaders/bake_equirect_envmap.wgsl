// node.bake_equirect_envmap — procedural HDR studio environment map.
//
// Equirectangular layout: longitude (azimuth) on X, latitude (elevation)
// on Y. Bakes the legacy MetallicGlass studio at adjustable resolution
// and brightness multipliers.
//
// Bindings:
//   @binding(0) uniforms (16 bytes — w + h + horizon_strength + azimuth_variation)
//   @binding(1) output_tex (rgba16float storage)

struct Uniforms {
    width: u32,
    height: u32,
    horizon_strength: f32,
    azimuth_variation: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var dst_tex: texture_storage_2d<rgba16float, write>;

const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318530;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= uniforms.width || gid.y >= uniforms.height { return; }

    let u_coord = f32(gid.x) / f32(uniforms.width);
    let v_coord = f32(gid.y) / f32(uniforms.height);

    let azimuth = u_coord * TAU - PI;
    let elevation = v_coord * PI - PI * 0.5;
    let up = sin(elevation);

    // Studio ambient floor
    var color = vec3<f32>(0.15, 0.15, 0.17);

    // Large bright horizon band (studio windows / white cyclorama)
    color += vec3<f32>(1.5, 1.45, 1.4) * exp(-15.0 * up * up) * uniforms.horizon_strength;

    // Overhead soft box
    let overhead = smoothstep(0.35, 0.65, up) * smoothstep(0.95, 0.65, up);
    color += vec3<f32>(2.5, 2.4, 2.3) * overhead;

    // Floor fill (bounced light from below)
    let floor_fill = smoothstep(-0.15, -0.45, up) * smoothstep(-0.85, -0.45, up);
    color += vec3<f32>(0.4, 0.42, 0.45) * floor_fill;

    // Two narrow strip lights (create chrome streaks)
    color += vec3<f32>(3.5, 3.2, 2.8) * exp(-300.0 * pow(up - 0.12, 2.0));
    color += vec3<f32>(1.5, 2.0, 3.0) * exp(-300.0 * pow(up + 0.08, 2.0));

    // Azimuthal variation — 1.0 + variation * sin(2 azimuth).
    color *= sin(azimuth * 2.0) * uniforms.azimuth_variation + 1.0;

    textureStore(dst_tex, vec2<i32>(gid.xy), vec4<f32>(color, 1.0));
}
