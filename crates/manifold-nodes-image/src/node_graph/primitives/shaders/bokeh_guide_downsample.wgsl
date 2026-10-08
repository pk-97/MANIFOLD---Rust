// Conservative source radii covering each color-mip footprint. Reading a
// level-zero radius beside a coarse color sample would erase thin coverage.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;
@compute @workgroup_size(16,16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(dst);
    if any(id.xy >= dims) { return; }
    let scale = vec2<f32>(textureDimensions(src))/vec2<f32>(dims);
    let lo = vec2<f32>(id.xy)*scale;
    let hi = lo+scale;
    var radii = vec4<f32>(0.0);
    for(var y=i32(floor(lo.y));y<i32(ceil(hi.y));y++) {
        for(var x=i32(floor(lo.x));x<i32(ceil(hi.x));x++) {
            radii = max(radii,textureLoad(src,vec2<i32>(x,y),0));
        }
    }
    textureStore(dst,vec2<i32>(id.xy),radii);
}
