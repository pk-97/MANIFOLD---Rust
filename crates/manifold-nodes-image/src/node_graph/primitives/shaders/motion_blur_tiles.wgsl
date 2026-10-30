// node.motion_blur pass 1: one workgroup per TILE x TILE block of output
// pixels, one thread per pixel. Each thread stores its pixel's smear length
// and inverse linear depth in `prep`, so every gather tap reads one texel.
// The group then reduces the block to its longest smear and the length of
// its shortest. Velocity and depth are read at each pixel's spot in their
// own grids, so a smaller buffer (temporal upscale) works unchanged.
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var velocity_tex: texture_2d<f32>;
@group(0) @binding(2) var depth_tex: texture_2d<f32>;
@group(0) @binding(3) var prep: texture_storage_2d<rg16float, write>;
@group(0) @binding(4) var tiles: texture_storage_2d<rgba16float, write>;

// xy: smear, z: its length.
var<workgroup> longest: array<vec3<f32>, 256>;
var<workgroup> shortest: array<f32, 256>;

@compute @workgroup_size(16, 16)
fn cs_main(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) group: vec3<u32>,
) {
    // Pixels past the frame edge repeat the last one, so every thread joins
    // the reduction and the barriers stay in uniform control flow.
    let last = vec2<u32>(u.dims) - 1u;
    let p = min(id.xy, last);
    let uv = (vec2<f32>(p) + 0.5) / u.dims;
    let ndc = textureLoad(velocity_tex, own_texel(uv, textureDimensions(velocity_tex)), 0).rg;
    let s = spread(ndc, u.dims, u.shutter_scale, u.max_blur_px);
    let len = length(s);
    if all(id.xy <= last) {
        let raw = textureLoad(depth_tex, own_texel(uv, textureDimensions(depth_tex)), 0).r;
        let inv_z = 1.0 / linearize_depth(raw, u.near, u.far);
        textureStore(prep, vec2<i32>(p), vec4<f32>(len, inv_z, 0.0, 0.0));
    }
    longest[li] = vec3<f32>(s, len);
    shortest[li] = len;
    workgroupBarrier();
    for (var stride = 128u; stride > 0u; stride >>= 1u) {
        if li < stride {
            let other = longest[li + stride];
            if other.z > longest[li].z {
                longest[li] = other;
            }
            shortest[li] = min(shortest[li], shortest[li + stride]);
        }
        workgroupBarrier();
    }
    if li == 0u {
        textureStore(tiles, vec2<i32>(group.xy), vec4<f32>(longest[0].xy, shortest[0], 0.0));
    }
}
