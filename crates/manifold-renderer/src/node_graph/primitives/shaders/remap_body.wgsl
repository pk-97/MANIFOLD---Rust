// node.remap — fusable body (freeze §12), the first GATHER atom. `source` is a
// Gather input: the body samples it at a coordinate it COMPUTES (a dependent
// sample), so the codegen passes the texture + sampler as args rather than a
// pre-read register. `uv_field` is Coincident (sampled at the fragment uv) and
// arrives as a colour register. Resample source at the UVs carried in
// uv_field.rg (Absolute) or at uv + that offset (Relative), with the wrap
// policy. Matches remap.wgsl. PARAMS: [wrap, mode] (both Enum -> u32).
fn wrap_coord(t: f32, mode: u32) -> f32 {
    if mode == 1u {
        // Repeat
        return fract(t);
    }
    if mode == 2u {
        // Mirror: triangle wave, period 2, peak 1.
        let m = fract(t * 0.5) * 2.0;
        return 1.0 - abs(1.0 - m);
    }
    // Clamp (default)
    return clamp(t, 0.0, 1.0);
}

fn body(source: texture_2d<f32>, samp: sampler, field_color: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, wrap: u32, mode: u32) -> vec4<f32> {
    let field = field_color.rg;
    let sample_uv = select(field, uv + field, mode == 1u);
    let wrapped = vec2<f32>(wrap_coord(sample_uv.x, wrap), wrap_coord(sample_uv.y, wrap));
    return textureSampleLevel(source, samp, wrapped, 0.0);
}
