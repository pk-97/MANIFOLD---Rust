// node.voronoi_cell_id — Voronoi partition that emits the F1-winning
// cell's integer coordinate (RG = cell_id.xy, B=0, A=1). Aspect-corrected
// (cells are square in pixels: scale = cell_count·aspect on X). hash2
// jitter. Verbatim partition from fx_voronoi_prism so a graph can rebuild
// the prism's cell layout, then hash the cell_id per beat (node.hash_
// field_by_seed) for per-cell content shuffle / visibility.

struct Uniforms {
    cell_count: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

fn hash2(p: vec2<f32>) -> vec2<f32> {
    let q = vec2<f32>(
        dot(p, vec2<f32>(127.1, 311.7)),
        dot(p, vec2<f32>(269.5, 183.3)),
    );
    return fract(sin(q) * 43758.5453);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let aspect_ratio = f32(dims.x) / f32(dims.y);

    let scaled_uv = uv * vec2<f32>(u.cell_count * aspect_ratio, u.cell_count);
    let cell_id = floor(scaled_uv);
    let cell_uv = fract(scaled_uv);

    var min_dist: f32 = 10.0;
    var nearest_cell_id: vec2<f32> = vec2<f32>(0.0, 0.0);

    for (var dy: i32 = -1; dy <= 1; dy++) {
        for (var dx: i32 = -1; dx <= 1; dx++) {
            let neighbor = vec2<f32>(f32(dx), f32(dy));
            let pt = hash2(cell_id + neighbor);
            let diff = neighbor + pt - cell_uv;
            let dist = dot(diff, diff);
            if dist < min_dist {
                min_dist = dist;
                nearest_cell_id = cell_id + neighbor;
            }
        }
    }

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(nearest_cell_id.x, nearest_cell_id.y, 0.0, 1.0));
}
