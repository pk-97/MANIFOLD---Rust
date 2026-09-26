// Area reduction preserves thin silhouettes and HDR energy at odd sizes.
// A normalized-UV bilinear fetch can miss texels entirely for 33 -> 16.
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var dst: texture_storage_2d<rgba16float, write>;
@compute @workgroup_size(16,16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(dst);
    if any(id.xy >= dims) { return; }
    let scale = vec2<f32>(textureDimensions(src))/vec2<f32>(dims);
    let lo = vec2<f32>(id.xy)*scale;
    let hi = lo+scale;
    var sum = vec4<f32>(0.0);
    for(var y=i32(floor(lo.y));y<i32(ceil(hi.y));y++) {
        for(var x=i32(floor(lo.x));x<i32(ceil(hi.x));x++) {
            let p = vec2<f32>(f32(x),f32(y));
            let overlap = max(min(hi,p+1.0)-max(lo,p),vec2<f32>(0.0));
            sum += textureLoad(src,vec2<i32>(x,y),0)*overlap.x*overlap.y;
        }
    }
    textureStore(dst,vec2<i32>(id.xy),sum/(scale.x*scale.y));
}
