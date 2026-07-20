// node.bake_equirect_envmap — procedural HDR studio environment map.
//
// Equirectangular layout: longitude (azimuth) on X, latitude (elevation)
// on Y.
//
// mode = 0 (gradient, default): legacy MetallicGlass studio look — bakes at
// adjustable resolution and brightness multipliers, byte-identical to the
// pre-D7 bake.
//
// mode = 1 (softbox, D7): pure-black base (every texel outside a strip's
// falloff band is EXACTLY 0.0 — Peter, 2026-07-15: "I want it PURE black
// void") lit only by `emitter_count` horizontal emitter strips, plus one
// optional directional sun disc (D7 sun-coherence addendum). Both the
// strips and the disc use compact-support falloff (smoothstep clamps to
// exactly 0.0/1.0 outside/inside their band) — never a Gaussian tail — so
// the "exact zero outside the lit regions" contract holds exactly, not
// merely approximately.
//
// Bindings:
//   @binding(0) uniforms (64 bytes)
//   @binding(1) output_tex (rgba16float storage)

struct Uniforms {
    width: u32,
    height: u32,
    horizon_strength: f32,
    azimuth_variation: f32,
    intensity: f32,
    mode: u32,
    emitter_count: u32,
    emitter_intensity: f32,
    emitter_elevation: f32,
    emitter_width: f32,
    sun_x: f32,
    sun_y: f32,
    sun_z: f32,
    sun_disc_intensity: f32,
    sun_disc_size: f32,
    // Softbox dome fill (IMPORT_FIDELITY F-P7): broad low-level studio
    // radiance so metals have a world to reflect. 0.0 = the original
    // pure-black-void softbox, byte-identical to pre-F-P7 bakes.
    fill_intensity: f32,
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

    var color: vec3<f32>;

    if uniforms.mode == 0u {
        // Studio ambient floor
        color = vec3<f32>(0.15, 0.15, 0.17);

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
    } else {
        // D7 — softbox: pure-black base. The ONLY light in the environment
        // is the emitter strips (+ optional sun disc) — nothing lifts the
        // shadows, so we start from exact zero, not a dim ambient term.
        //
        // F-P7 dome fill: at fill_intensity 0 the base stays EXACTLY 0.0
        // (the D7 contract, byte-identical). Above 0 it adds a broad
        // neutral-white studio dome — brighter overhead, dimmer toward
        // the floor, never zero anywhere — because metals are lit
        // exclusively by the environment: against a black void every
        // metallic import reads as dark chrome no matter its albedo.
        // The strips keep
        // supplying the specular streaks; the fill supplies the world.
        color = vec3<f32>(1.0, 0.985, 0.96)
            * uniforms.fill_intensity
            * (0.55 + 0.45 * clamp(up, -1.0, 1.0));

        let half_width = max(uniforms.emitter_width, 1e-4);
        let spacing = half_width * 4.0;
        let n = max(uniforms.emitter_count, 1u);
        let mid = (f32(n) - 1.0) * 0.5;
        // Strips accumulate separately: `emitter_intensity` scales the
        // strips ONLY, never the fill dome (they are independent faders
        // on the import card — coupling them was the F-P7 first-cut bug:
        // strip intensity 0 blacked out the entire environment).
        var strips = vec3<f32>(0.0, 0.0, 0.0);
        for (var i: u32 = 0u; i < n; i = i + 1u) {
            let center = uniforms.emitter_elevation + (f32(i) - mid) * spacing;
            let dist = abs(up - center);
            // Compact support: smoothstep clamps to EXACTLY 1.0 once
            // dist >= half_width, so `core` is EXACTLY 0.0 beyond the
            // falloff band (D7's "exact zero outside strips" contract).
            let core = 1.0 - smoothstep(half_width * 0.5, half_width, dist);
            strips += vec3<f32>(3.0, 2.8, 2.5) * core;
        }
        color += strips * uniforms.emitter_intensity;

        // Sun disc (D7 sun-coherence addendum) — directional only; a sun is
        // infinitely far, which an equirect envmap represents exactly.
        // Point lights are near-field and must NOT be painted here.
        //
        // Skipped entirely when intensity is 0 (byte-identical to no disc —
        // the F-P3 gate) and when the direction is degenerate (avoids a NaN
        // from normalizing a zero vector when the importer hasn't bound a
        // direction yet).
        if uniforms.sun_disc_intensity > 0.0 {
            let sun_dir_raw = vec3<f32>(uniforms.sun_x, uniforms.sun_y, uniforms.sun_z);
            if dot(sun_dir_raw, sun_dir_raw) > 1e-10 {
                let sun_dir = normalize(sun_dir_raw);
                // Fragment direction on the unit sphere, in the SAME
                // convention as pbr_brdf.wgsl's `pbr_equirect_uv` inverse:
                // x = cos(elev)*cos(az), y = sin(elev), z = cos(elev)*sin(az).
                let cos_elev = cos(elevation);
                let frag_dir = vec3<f32>(cos_elev * cos(azimuth), up, cos_elev * sin(azimuth));
                let d = dot(frag_dir, sun_dir);
                let outer = cos(max(uniforms.sun_disc_size, 1e-4));
                // Compact support again: d <= outer -> EXACTLY 0.0. Unique
                // maximum at d = 1.0 (frag_dir == sun_dir) so the brightest
                // texel sits precisely at the sun direction.
                let disc = smoothstep(outer, 1.0, d);
                color += vec3<f32>(4.0, 4.0, 3.8) * uniforms.sun_disc_intensity * disc;
            }
        }
    }

    // Master brightness over every studio term — 0 bakes a fully black map so
    // PBR objects get no image-based lighting (lit only by their scene lights).
    color *= uniforms.intensity;

    textureStore(dst_tex, vec2<i32>(gid.xy), vec4<f32>(color, 1.0));
}
