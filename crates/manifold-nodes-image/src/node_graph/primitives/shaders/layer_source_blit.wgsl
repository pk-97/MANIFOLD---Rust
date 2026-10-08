// Copy the previous-frame layer composite from the layer-skin registry into
// the chain-allocated output texture. Same shape as hdri_source_blit.wgsl —
// a plain resample, no aspect fit: the registry texture is canvas-sized and
// the output slot matches (output_dims returns the canvas dims), so the
// normal case is a 1:1 copy; the resample exists so a host rendering at a
// different canvas (thumbnails, previews) gets a clean stretch instead of a
// size-assert panic. Content is linear HDR compositor output — pure numeric
// resample, no gamma handling.

struct Uniforms {
    out_width: f32,
    out_height: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<u32>(u32(u.out_width), u32(u.out_height));
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let c = textureSampleLevel(src_tex, src_sampler, uv, 0.0);
    textureStore(output_tex, vec2<i32>(gid.xy), c);
}
