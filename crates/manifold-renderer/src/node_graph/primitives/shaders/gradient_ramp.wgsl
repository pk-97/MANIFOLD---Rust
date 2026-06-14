// node.gradient_ramp — N-stop piecewise-linear gradient → LUT texture.
//
// Output texel x maps to t = x / (width - 1) * domain — ENDPOINT-inclusive, so
// texel 0 holds t=0 (the first stop, e.g. pure black) and the last texel holds
// t=domain. A pure-black input therefore reads back pure black through
// node.color_lut; a centre mapping (x+0.5)/width would leave texel 0 a half-step
// up the ramp (a faint hue on the darkest palettes). Matches legacy i/(N-1).
// The gradient is
// evaluated over `count` stops (each vec4 = (position, r, g, b)) and matches
// the legacy Infrared gradient() exactly:
//   - t <= first stop position  → the first stop's colour (clamp below).
//   - between two stops          → linear interpolation.
//   - t > last stop position     → EXTRAPOLATE the last segment (overshoot).
//     This is what produces the HDR blowout highlights past luma 1.0.
//
// The gradient is 1D in x (every row identical), so the output is a luminance
// LUT that node.color_lut can sample directly, and is reusable as a gradient
// texture anywhere a ramp is wanted.
//
// Bindings:
//   @binding(0) uniforms (16-byte header + 16 vec4 stops)
//   @binding(1) output_tex (rgba16float storage)

struct Uniforms {
    count: u32,
    domain: f32,
    _pad0: u32,
    _pad1: u32,
    stops: array<vec4<f32>, 16>, // (position, r, g, b)
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let t = f32(id.x) / max(f32(dims.x) - 1.0, 1.0) * u.domain;

    let n = max(u.count, 1u);
    var rgb = u.stops[0].yzw;

    if t > u.stops[0].x && n >= 2u {
        var found = false;
        for (var i = 1u; i < n; i = i + 1u) {
            if t <= u.stops[i].x {
                let p0 = u.stops[i - 1u].x;
                let p1 = u.stops[i].x;
                let s = (t - p0) / (p1 - p0);
                rgb = mix(u.stops[i - 1u].yzw, u.stops[i].yzw, s);
                found = true;
                break;
            }
        }
        if !found {
            // Extrapolate beyond the last stop using the last segment's
            // direction — matches the legacy gradient() overshoot.
            let p0 = u.stops[n - 2u].x;
            let p1 = u.stops[n - 1u].x;
            let s = (t - p0) / (p1 - p0);
            rgb = mix(u.stops[n - 2u].yzw, u.stops[n - 1u].yzw, s);
        }
    }

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(rgb, 1.0));
}
