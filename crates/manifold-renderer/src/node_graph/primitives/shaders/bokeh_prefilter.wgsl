// Classify each full-resolution texel BEFORE reducing. RGB is premultiplied
// by layer coverage. Camera input is already premultiplied; multiplying it
// by opacity again would darken partially covered silhouettes (BUG-imds).
// Exact loads preserve one-pixel silhouettes and categorical near/far flags.
@group(0) @binding(0) var color: texture_2d<f32>;
@group(0) @binding(1) var coc: texture_2d<f32>;
@group(0) @binding(2) var far_color: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var near_color: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var guide: texture_storage_2d<rgba16float, write>;
struct Uniforms { radius: f32, blur_alpha: u32, _pad0: f32, _pad1: f32 }
@group(0) @binding(5) var<uniform> u: Uniforms;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(guide)) { return; }
    let color_dims = textureDimensions(color);
    let coc_dims = textureDimensions(coc);
    var far = vec4<f32>(0.0);
    var near = vec4<f32>(0.0);
    var radii = vec2<f32>(0.0);
    for (var y = 0u; y < 2u; y++) {
        for (var x = 0u; x < 2u; x++) {
            let p = min(id.xy * 2u + vec2<u32>(x,y), color_dims - 1u);
            let cp = min(vec2<u32>((vec2<f32>(p) + 0.5) * vec2<f32>(coc_dims) / vec2<f32>(color_dims)), coc_dims - 1u);
            let c = textureLoad(color, vec2<i32>(p), 0);
            let opacity = select(1.0,clamp(c.a,0.0,1.0),u.blur_alpha != 0u);
            let rgb = select(c.rgb,vec3<f32>(0.0),u.blur_alpha != 0u && opacity == 0.0);
            let premul = vec4<f32>(rgb,opacity);
            let z = textureLoad(coc, vec2<i32>(cp), 0);
            let r = select(0.0,clamp(z.r, 0.0, 1.0),opacity > 0.0);
            if z.g >= 0.5 && r > 0.0 {
                near += premul * 0.25;
                radii.y = max(radii.y, r);
            } else {
                far += premul * 0.25;
                radii.x = max(radii.x, r);
            }
        }
    }
    textureStore(far_color, vec2<i32>(id.xy), far);
    textureStore(near_color, vec2<i32>(id.xy), near);
    textureStore(guide, vec2<i32>(id.xy), vec4<f32>(radii, far.a, near.a));
}
