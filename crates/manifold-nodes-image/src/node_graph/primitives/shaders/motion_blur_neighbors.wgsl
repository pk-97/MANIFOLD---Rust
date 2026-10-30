// node.motion_blur pass 2: the longest smear that reaches each tile from
// any tile within reach_tiles, and the shortest smear length among the tiles
// that longest smear spans. A pixel next to a moving object takes the
// object's smear from here, so the blur spreads past the object's edge; a
// neighbourhood whose shortest smear is close to its longest moves as one.
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tiles: texture_2d<f32>;
@group(0) @binding(2) var neighbors: texture_storage_2d<rgba16float, write>;

// Gap between this tile's rectangle and the one (x, y) tiles away, in
// pixels. Adjacent tiles touch, so their gap is zero.
fn gap(x: i32, y: i32) -> f32 {
    return length(max(abs(vec2<f32>(f32(x), f32(y))) - 1.0, vec2<f32>(0.0))) * f32(TILE);
}

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
            let g = gap(x, y);
            if len2 > best_len2 && len2 >= g * g {
                best = s;
                best_len2 = len2;
            }
        }
    }
    // Taps land up to the longest smear away, plus the quarter tile the
    // gather's jittered lookup may shift by.
    let span = sqrt(best_len2) + f32(TILE) * 0.25;
    var shortest = 65504.0;
    for (var y = -r; y <= r; y++) {
        for (var x = -r; x <= r; x++) {
            let p = vec2<i32>(id.xy) + vec2<i32>(x, y);
            if any(p < vec2<i32>(0)) || any(p >= vec2<i32>(dims)) || gap(x, y) > span {
                continue;
            }
            shortest = min(shortest, textureLoad(tiles, p, 0).b);
        }
    }
    textureStore(neighbors, vec2<i32>(id.xy), vec4<f32>(best, shortest, 0.0));
}
