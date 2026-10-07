// node.sample_volume_2d — sample a Texture3D at a fixed Z slice
// (and optional UV transform) to produce a Texture2D. New WGSL
// for the buffer-port primitive vocabulary; the legacy
// mri_slice_compute.wgsl samples 2D textures of already-CPU-loaded
// slices, not a 3D volume.
//
// `slice_z` is the normalised z coordinate in [0, 1]. UV scale +
// center allow re-framing the slice into the output texture; the
// output texture's dimensions drive the dispatch grid.

struct SampleVolumeUniforms {
    slice_z: f32,
    uv_scale: f32,
    center_x: f32,
    center_y: f32,
};

@group(0) @binding(0) var<uniform> u: SampleVolumeUniforms;
@group(0) @binding(1) var volume: texture_3d<f32>;
@group(0) @binding(2) var s_volume: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }

    let uv_raw = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let centered = uv_raw - vec2<f32>(0.5);
    let uv = centered / max(u.uv_scale, 0.001)
           + vec2<f32>(u.center_x + 0.5, u.center_y + 0.5);

    let sampled = textureSampleLevel(
        volume,
        s_volume,
        vec3<f32>(uv.x, uv.y, clamp(u.slice_z, 0.0, 1.0)),
        0.0,
    );

    textureStore(output_tex, vec2<i32>(gid.xy), sampled);
}
