// Cross-format texture copy for `node.feedback`'s Phase 3c path.
//
// Used when the wire entering feedback's `in` port carries a different
// pixel format than feedback's persistent state texture — typically
// fp16 intermediates feeding an fp32 state. Metal's blit encoder
// requires matching pixel formats, so we route this case through a
// compute dispatch that samples the source (any sampleable format via
// `texture_2d<f32>`) and writes the destination at the storage
// texture's declared format.
//
// One shader variant per destination format. This file targets
// rgba32float dst; add sibling files (or a build-time generated set)
// for other dst formats as new feedback-using presets need them.

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var dst_tex: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(16, 16, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst_tex);
    if gid.x >= dims.x || gid.y >= dims.y { return; }
    // textureLoad picks up the source pixel at integer coords without
    // a sampler. Source and destination dims must match — feedback's
    // run() enforces this before dispatching.
    let v = textureLoad(src_tex, vec2<i32>(gid.xy), 0);
    textureStore(dst_tex, vec2<i32>(gid.xy), v);
}
