// Black Hole — Scatter Resolve
//
// Converts atomic accumulator → RGBA density texture + self-clear.
// Separate file from scatter to avoid naga uniform size mismatch.

struct ResolveUniforms {
    tex_w: u32,
    tex_h: u32,
    disk_inner: f32,
    disk_outer: f32,
};

@group(0) @binding(0) var<storage, read_write> accum: array<atomic<u32>>;
@group(0) @binding(1) var disk_density: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> params: ResolveUniforms;

@compute @workgroup_size(16, 16)
fn resolve(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.tex_w || gid.y >= params.tex_h {
        return;
    }

    let idx = gid.y * params.tex_w + gid.x;
    let raw = atomicLoad(&accum[idx]);
    let density = f32(raw) / 4096.0;

    // Color based on radial position (Y axis = radius)
    let r_norm = f32(gid.y) / f32(params.tex_h);

    // Temperature gradient
    let inner_col = vec3<f32>(1.0, 0.95, 0.85);
    let mid_col = vec3<f32>(1.0, 0.55, 0.15);
    let outer_col = vec3<f32>(0.6, 0.12, 0.02);
    var col: vec3<f32>;
    if r_norm < 0.5 {
        col = mix(inner_col, mid_col, r_norm * 2.0);
    } else {
        col = mix(mid_col, outer_col, (r_norm - 0.5) * 2.0);
    }

    // Apply density as emission intensity
    let intensity = density * (1.0 + (1.0 - r_norm) * 3.0);
    col *= intensity;

    textureStore(disk_density, gid.xy, vec4<f32>(col, density));

    // Self-clear for next frame
    atomicStore(&accum[idx], 0u);
}
