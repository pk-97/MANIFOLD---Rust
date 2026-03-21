// MRI Volume — Slice Viewer
// Samples an arbitrary axis-aligned plane through a 3D volume texture.
// Applies window/level tone mapping + unsharp mask sharpening.

struct Uniforms {
    slice_axis: f32,     // 0 = Axial (Z), 1 = Sagittal (X), 2 = Coronal (Y)
    slice_pos: f32,      // 0..1 normalized position along slice axis
    window_center: f32,  // 0..1 normalized
    window_width: f32,   // 0.01..1.0 normalized
    aspect_ratio: f32,   // output width / height
    uv_scale: f32,       // 1.0 / scale param
    invert: f32,         // 0.0 or 1.0
    spacing_x: f32,
    spacing_y: f32,
    spacing_z: f32,
    dim_x: f32,
    dim_y: f32,
    dim_z: f32,
    sharpen: f32,        // sharpening strength (0 = off, 1 = default, 2 = strong)
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var volume_tex: texture_3d<f32>;
@group(0) @binding(2) var volume_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// Map 2D UV to 3D texture coordinates for the current slice axis
fn map_uvw(uv_in: vec2<f32>, axis: u32) -> vec3<f32> {
    let vy = 1.0 - uv_in.y;
    if axis == 0u {
        return vec3<f32>(uv_in.x, vy, u.slice_pos);
    } else if axis == 1u {
        return vec3<f32>(u.slice_pos, uv_in.x, vy);
    } else {
        return vec3<f32>(uv_in.x, u.slice_pos, vy);
    }
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = (in.uv - 0.5) * u.uv_scale + 0.5;

    if uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let axis = u32(u.slice_axis + 0.5);
    let uvw = map_uvw(uv, axis);

    // Sample center
    let center = textureSampleLevel(volume_tex, volume_sampler, uvw, 0.0).r;

    // Unsharp mask: sample 4 neighbors, compute Laplacian, sharpen
    var sharpened = center;
    if u.sharpen > 0.0 {
        // Texel size in UV space for the visible plane
        var dx: vec2<f32>;
        if axis == 0u {
            dx = vec2<f32>(1.0 / u.dim_x, 1.0 / u.dim_y);
        } else if axis == 1u {
            dx = vec2<f32>(1.0 / u.dim_y, 1.0 / u.dim_z);
        } else {
            dx = vec2<f32>(1.0 / u.dim_x, 1.0 / u.dim_z);
        }

        let s_l = textureSampleLevel(volume_tex, volume_sampler, map_uvw(uv + vec2<f32>(-dx.x, 0.0), axis), 0.0).r;
        let s_r = textureSampleLevel(volume_tex, volume_sampler, map_uvw(uv + vec2<f32>( dx.x, 0.0), axis), 0.0).r;
        let s_u = textureSampleLevel(volume_tex, volume_sampler, map_uvw(uv + vec2<f32>(0.0, -dx.y), axis), 0.0).r;
        let s_d = textureSampleLevel(volume_tex, volume_sampler, map_uvw(uv + vec2<f32>(0.0,  dx.y), axis), 0.0).r;

        // Laplacian = 4*center - (left + right + up + down)
        let laplacian = 4.0 * center - (s_l + s_r + s_u + s_d);
        sharpened = center + laplacian * u.sharpen * 0.5;
    }

    // Window/level tone mapping
    let w_low = u.window_center - u.window_width * 0.5;
    let w_high = u.window_center + u.window_width * 0.5;
    var lum = clamp((sharpened - w_low) / max(w_high - w_low, 0.001), 0.0, 1.0);

    // Subtle contrast curve (S-curve) for extra pop
    lum = lum * lum * (3.0 - 2.0 * lum);

    lum = mix(lum, 1.0 - lum, u.invert);

    return vec4<f32>(lum, lum, lum, 1.0);
}
