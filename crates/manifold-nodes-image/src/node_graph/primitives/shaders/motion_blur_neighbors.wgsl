// node.motion_blur pass 2: the longest smear that reaches each tile from
// any tile within reach_tiles. A pixel next to a moving object takes the
// object's smear from here, so the blur spreads past the object's edge.
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tiles: texture_2d<f32>;
@group(0) @binding(2) var neighbors: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(neighbors);
    if any(id.xy >= dims) {
        return;
    }
    let r = i32(u.reach_tiles);
    var best = vec2<f32>(0.0);
    var best_len2 = 0.0;
    for (var y = -r; y <= r; y++) {
        for (var x = -r; x <= r; x++) {
            let p = vec2<i32>(id.xy) + vec2<i32>(x, y);
            if any(p < vec2<i32>(0)) || any(p >= vec2<i32>(dims)) {
                continue;
            }
            let s = textureLoad(tiles, p, 0).rg;
            let len2 = dot(s, s);
            // Gap between the two tiles' rectangles in pixels. Adjacent
            // tiles touch, so their gap is zero and they always count.
            let gap = length(max(abs(vec2<f32>(f32(x), f32(y))) - 1.0, vec2<f32>(0.0))) * f32(TILE);
            if len2 > best_len2 && len2 >= gap * gap {
                best = s;
                best_len2 = len2;
            }
        }
    }
    textureStore(neighbors, vec2<i32>(id.xy), vec4<f32>(best, 0.0, 0.0));
}
