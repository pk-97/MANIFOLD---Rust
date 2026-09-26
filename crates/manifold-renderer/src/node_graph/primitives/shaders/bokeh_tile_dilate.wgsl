struct Uniforms { radius: f32, _pad0: f32, _pad1: f32, _pad2: f32 }
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tiles: texture_2d<f32>;
@group(0) @binding(2) var reach: texture_storage_2d<rgba16float, write>;
@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(reach);
    if any(id.xy >= dims) { return; }
    let radius_tiles = min(i32(ceil(min(u.radius*1.5,f32(max(dims.x,dims.y))*8.0)/8.0))+1,i32(max(dims.x,dims.y)));
    var r = vec2<f32>(0.0);
    for (var y = -radius_tiles; y <= radius_tiles; y++) {
        for (var x = -radius_tiles; x <= radius_tiles; x++) {
            let p = vec2<i32>(id.xy) + vec2<i32>(x,y);
            if any(p < vec2<i32>(0)) || any(p >= vec2<i32>(dims)) { continue; }
            let candidate = textureLoad(tiles, p, 0).rg;
            // Distance between tile rectangles; a neighboring tile starts
            // at zero distance. Include one half-pixel for reconstruction.
            let distance = length(max(abs(vec2<f32>(f32(x),f32(y))) - 1.0, vec2<f32>(0.0))) * 8.0;
            // Polygon corners and mip filtering extend beyond the ideal disc.
            r = max(r, select(vec2<f32>(0.0), candidate, candidate * u.radius * 1.5 + 1.0 >= vec2<f32>(distance)));
        }
    }
    textureStore(reach, vec2<i32>(id.xy), vec4<f32>(r, 0.0, 0.0));
}
