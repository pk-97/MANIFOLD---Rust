// node.gradient_ramp — fusable body (freeze §12), SOURCE with a TABLE param.
// N-stop piecewise-linear gradient → LUT texture. Output column x maps to
// t = (x+0.5)/width * domain, which the body recovers as uv.x * domain (uv.x is
// exactly (x+0.5)/width). Evaluated over `stops_count` stops (each vec4 =
// (position, r, g, b)): clamp below the first stop, lerp between stops, and
// EXTRAPOLATE the last segment past the last stop (the HDR-overshoot tail). The
// `stops` Table param expands in the generated uniform to a `_count` header word
// plus a fixed array<vec4<f32>, 16>; the body receives (stops_count, stops).
// Matches gradient_ramp.wgsl. PARAMS: [stops (Table), domain].
fn body(uv: vec2<f32>, dims: vec2<f32>, domain: f32, stops_count: u32, stops: array<vec4<f32>, 16>) -> vec4<f32> {
    let t = uv.x * domain;

    let n = max(stops_count, 1u);
    var rgb = stops[0].yzw;

    if t > stops[0].x && n >= 2u {
        var found = false;
        for (var i = 1u; i < n; i = i + 1u) {
            if t <= stops[i].x {
                let p0 = stops[i - 1u].x;
                let p1 = stops[i].x;
                let s = (t - p0) / (p1 - p0);
                rgb = mix(stops[i - 1u].yzw, stops[i].yzw, s);
                found = true;
                break;
            }
        }
        if !found {
            // Extrapolate beyond the last stop using the last segment's
            // direction — matches the legacy gradient() overshoot.
            let p0 = stops[n - 2u].x;
            let p1 = stops[n - 1u].x;
            let s = (t - p0) / (p1 - p0);
            rgb = mix(stops[n - 2u].yzw, stops[n - 1u].yzw, s);
        }
    }

    return vec4<f32>(rgb, 1.0);
}
