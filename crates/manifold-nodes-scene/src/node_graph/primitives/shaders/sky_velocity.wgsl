// node.render_scene: pixels where nothing drew (reversed-Z depth 0) get the
// camera's own turn since last frame, so a motion blur reading the velocity
// output smears the sky on a pan like the rest of the scene. A direction at
// infinity moves with the camera's rotation only: transforming it with w = 0
// drops the translation.
struct Uniforms {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    // xy: this frame's jitter, zw: last frame's, as NDC offsets. Velocity
    // excludes jitter, as every other velocity writer does.
    jitter: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var depth_tex: texture_2d<f32>;
@group(0) @binding(2) var velocity_out: texture_storage_2d<rg16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(velocity_out);
    if any(id.xy >= dims) {
        return;
    }
    if textureLoad(depth_tex, vec2<i32>(id.xy), 0).r > 0.0 {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let near = u.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let far = u.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let dir = far.xyz / far.w - near.xyz / near.w;
    let prev = u.prev_view_proj * vec4<f32>(dir, 0.0);
    // Behind last frame's camera: a turn too large to smear.
    if prev.w <= 1e-6 {
        return;
    }
    let motion = ndc - prev.xy / prev.w - (u.jitter.xy - u.jitter.zw);
    textureStore(velocity_out, vec2<i32>(id.xy), vec4<f32>(motion, 0.0, 0.0));
}
