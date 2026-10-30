// node.bake_environment, mode = HDRI: the wired `hdri` texture scaled by
// `intensity`. The output is allocated at the HDRI's own size, so this is a
// 1:1 copy; the clamped scaling only covers a size mismatch during a resize.
//
// With a fill radius, the sun inside it is painted out for the lighting map:
// each texel there takes the sky mirrored across the fill's edge, along the
// great circle through the sun, so the sky around the sun closes over it.
//
// Bindings:
//   @binding(0) uniforms (32 bytes)
//   @binding(1) dst_tex (rgba16float storage)
//   @binding(2) src_tex (the HDRI, row 0 at the nadir)

const PI: f32 = 3.14159265;

struct Uniforms {
    intensity: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    // xyz: toward the sun in the file's frame; w: the angular radius
    // filled, radians (0 copies as it is).
    fill: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var dst_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var src_tex: texture_2d<f32>;

// The env convention (`env_lookup_uv` with no turn): x = cos(lat)cos(lon),
// y = sin(lat), z = cos(lat)sin(lon).
fn texel_direction(uv: vec2<f32>) -> vec3<f32> {
    let lon = (uv.x - 0.5) * 2.0 * PI;
    let lat = (uv.y - 0.5) * PI;
    return vec3<f32>(cos(lat) * cos(lon), sin(lat), cos(lat) * sin(lon));
}

fn direction_texel(dir: vec3<f32>, dims: vec2<u32>) -> vec2<u32> {
    let uv = vec2<f32>(atan2(dir.z, dir.x) / (2.0 * PI) + 0.5,
        asin(clamp(dir.y, -1.0, 1.0)) / PI + 0.5);
    let texel = vec2<u32>(clamp(uv * vec2<f32>(dims), vec2<f32>(0.0), vec2<f32>(dims) - 1.0));
    return texel;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dst_dims = textureDimensions(dst_tex);
    if gid.x >= dst_dims.x || gid.y >= dst_dims.y { return; }
    let src_dims = textureDimensions(src_tex);
    let scaled = vec2<u32>(vec2<f32>(gid.xy) * vec2<f32>(src_dims) / vec2<f32>(dst_dims));
    var texel = min(scaled, src_dims - vec2<u32>(1u, 1u));
    let radius = uniforms.fill.w;
    if radius > 0.0 {
        let sun = uniforms.fill.xyz;
        let dir = texel_direction((vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dst_dims));
        let along = dot(dir, sun);
        if along > cos(radius) {
            let theta = acos(clamp(along, -1.0, 1.0));
            let side = dir - sun * along;
            var away = vec3<f32>(0.0);
            if length(side) > 1e-6 {
                away = normalize(side);
            } else {
                // The sun's own centre: any way out will do.
                let up = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(sun.y) > 0.9);
                away = normalize(cross(sun, up));
            }
            let mirrored = 2.0 * radius - theta;
            texel = direction_texel(cos(mirrored) * sun + sin(mirrored) * away, src_dims);
        }
    }
    let radiance = textureLoad(src_tex, vec2<i32>(texel), 0).rgb;
    textureStore(dst_tex, vec2<i32>(gid.xy), vec4<f32>(radiance * uniforms.intensity, 1.0));
}
