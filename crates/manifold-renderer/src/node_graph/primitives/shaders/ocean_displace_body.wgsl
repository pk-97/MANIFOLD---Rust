// node.ocean_displace — fusable BUFFER body. Moves each vertex by three ocean
// cascades sampled at its rest position (uv, metres) and paints foam where
// the summed surface folds (docs/OCEAN_SURFACE_DESIGN.md D6, D9).
//
// ABI (buffer standalone codegen): `mesh` is coincident (e_mesh); field_0..2
// are BUFFER GATHER inputs bound as globals buf_field_0..2: array<f32>, each
// six N×N real fields, field-major then row (z) then column (x). Params in
// PARAMS order, then the derived camera x/z. Int params arrive as i32.

struct OdpTaps {
    i00: u32,
    i10: u32,
    i01: u32,
    i11: u32,
    w: vec2<f32>,
}

// Bilinear taps with wrap for world point p on an N×N tile of size l.
fn odp_taps(n: u32, l: f32, p: vec2<f32>) -> OdpTaps {
    let t = p / max(l, 1e-3) * f32(n);
    let base = floor(t);
    let ni = i32(n);
    let x0 = ((i32(base.x) % ni) + ni) % ni;
    let z0 = ((i32(base.y) % ni) + ni) % ni;
    let x1 = (x0 + 1) % ni;
    let z1 = (z0 + 1) % ni;
    return OdpTaps(
        u32(z0 * ni + x0),
        u32(z0 * ni + x1),
        u32(z1 * ni + x0),
        u32(z1 * ni + x1),
        t - base,
    );
}

fn odp_fade(start: f32, end: f32, d: f32) -> f32 {
    return 1.0 - smoothstep(start, max(end, start + 1e-3), d);
}

fn body(
    idx: u32,
    count: u32,
    e_mesh: Element,
    choppiness: f32,
    foam_threshold: f32,
    foam_width: f32,
    size_0: i32,
    tile_size_0: f32,
    fade_start_0: f32,
    fade_end_0: f32,
    size_1: i32,
    tile_size_1: f32,
    fade_start_1: f32,
    fade_end_1: f32,
    size_2: i32,
    tile_size_2: f32,
    fade_start_2: f32,
    fade_end_2: f32,
    cam_x: f32,
    cam_z: f32,
) -> Element {
    var v = e_mesh;
    let rest = v.uv;
    let d = length(rest - vec2<f32>(cam_x, cam_z));
    // Σ fade·(λDx, Dy, λDz) and Σ fade·λ·(Dxx, Dzz, Dxz).
    var disp = vec3<f32>(0.0);
    var jac = vec3<f32>(0.0);

    let f0 = odp_fade(fade_start_0, fade_end_0, d);
    if f0 > 0.0 {
        let n = u32(size_0);
        let t = odp_taps(n, tile_size_0, rest);
        var s: array<f32, 6>;
        for (var f = 0u; f < 6u; f = f + 1u) {
            let o = f * n * n;
            let a = mix(buf_field_0[o + t.i00], buf_field_0[o + t.i10], t.w.x);
            let b = mix(buf_field_0[o + t.i01], buf_field_0[o + t.i11], t.w.x);
            s[f] = mix(a, b, t.w.y);
        }
        disp += f0 * vec3<f32>(choppiness * s[1], s[0], choppiness * s[2]);
        jac += f0 * choppiness * vec3<f32>(s[3], s[4], s[5]);
    }

    let f1 = odp_fade(fade_start_1, fade_end_1, d);
    if f1 > 0.0 {
        let n = u32(size_1);
        let t = odp_taps(n, tile_size_1, rest);
        var s: array<f32, 6>;
        for (var f = 0u; f < 6u; f = f + 1u) {
            let o = f * n * n;
            let a = mix(buf_field_1[o + t.i00], buf_field_1[o + t.i10], t.w.x);
            let b = mix(buf_field_1[o + t.i01], buf_field_1[o + t.i11], t.w.x);
            s[f] = mix(a, b, t.w.y);
        }
        disp += f1 * vec3<f32>(choppiness * s[1], s[0], choppiness * s[2]);
        jac += f1 * choppiness * vec3<f32>(s[3], s[4], s[5]);
    }

    let f2 = odp_fade(fade_start_2, fade_end_2, d);
    if f2 > 0.0 {
        let n = u32(size_2);
        let t = odp_taps(n, tile_size_2, rest);
        var s: array<f32, 6>;
        for (var f = 0u; f < 6u; f = f + 1u) {
            let o = f * n * n;
            let a = mix(buf_field_2[o + t.i00], buf_field_2[o + t.i10], t.w.x);
            let b = mix(buf_field_2[o + t.i01], buf_field_2[o + t.i11], t.w.x);
            s[f] = mix(a, b, t.w.y);
        }
        disp += f2 * vec3<f32>(choppiness * s[1], s[0], choppiness * s[2]);
        jac += f2 * choppiness * vec3<f32>(s[3], s[4], s[5]);
    }

    v.position = v.position + disp;
    let j = (1.0 + jac.x) * (1.0 + jac.y) - jac.z * jac.z;
    let foam = 1.0 - smoothstep(foam_threshold - max(foam_width, 1e-3), foam_threshold, j);
    v.color = vec4<f32>(mix(v.color.rgb, vec3<f32>(1.0), foam), v.color.a);
    return v;
}
