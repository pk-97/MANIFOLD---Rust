// node.render_scene internal pass (VOLUMETRIC_LIGHT_DESIGN.md D3, P2) — half-res
// point-sample downsample of the resolved scene depth (R32Float raw [0,1] clip
// depth). Feeds both the march kernel (ray-endpoint depth) and the
// upsample-composite pass (depth-similarity weights). Point sample, not
// min/max — "no min/max depth puzzle in v1" (D3). Internal to render_scene,
// not a graph atom — same exemption class as `ensure_shadow_pass`'s
// hand-written shadow_depth.wgsl pipeline (§2.5 audit: zero new graph
// primitives).

@group(0) @binding(0) var full_depth: texture_2d<f32>;
@group(0) @binding(1) var half_depth_out: texture_storage_2d<r32float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let half_dims = textureDimensions(half_depth_out);
    if id.x >= u32(half_dims.x) || id.y >= u32(half_dims.y) {
        return;
    }
    let full_dims = textureDimensions(full_depth);
    let src = vec2<i32>(
        clamp(i32(id.x) * 2, 0, i32(full_dims.x) - 1),
        clamp(i32(id.y) * 2, 0, i32(full_dims.y) - 1),
    );
    let raw = textureLoad(full_depth, src, 0).r;
    textureStore(half_depth_out, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(raw, 0.0, 0.0, 0.0));
}
