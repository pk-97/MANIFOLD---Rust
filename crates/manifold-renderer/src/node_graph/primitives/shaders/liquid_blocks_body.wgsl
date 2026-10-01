// node.liquid_blocks — fusable BUFFER body, GATHER. One thread per 4³ block
// of the domain's cells (the solid lattice read as nodes − 1 cells a side).
// LIQUID: some cell in the block has water > 0. SURFACE: the refined level
// set has a node < 0 and a node >= 0 among refined nodes 4bs to
// min(4(b+1)s, L − 1) per axis (the closed footprint of the block's cells).
// SOLID: some solid node < 0 among nodes 4b to min(4(b+1), N − 1). A lattice
// past its arrays, or a refinement that is not whole, gives 0.

fn body(
    idx: u32,
    count: u32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    level_nodes_x: f32,
    level_nodes_y: f32,
    level_nodes_z: f32,
) -> u32 {
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    let levels = vec3<u32>(max(vec3<f32>(level_nodes_x, level_nodes_y, level_nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) || any(levels < vec3<u32>(2u)) {
        return 0u;
    }
    let cells = nodes - vec3<u32>(1u);
    let s = (levels.x - 1u) / cells.x;
    let blocks = lb_blocks(cells);
    if s < 1u || s > 4u || any(levels - vec3<u32>(1u) != cells * s)
        || idx >= blocks.x * blocks.y * blocks.z
        || cells.x * cells.y * cells.z > arrayLength(&buf_water)
        || nodes.x * nodes.y * nodes.z > arrayLength(&buf_solid)
        || levels.x * levels.y * levels.z > arrayLength(&buf_level_set) {
        return 0u;
    }
    let b = vec3<u32>(idx % blocks.x, (idx / blocks.x) % blocks.y, idx / (blocks.x * blocks.y));
    let first = b * LB_CELLS;
    let end = min(first + vec3<u32>(LB_CELLS), cells);
    var bits = 0u;
    for (var z = first.z; z < end.z; z = z + 1u) {
        for (var y = first.y; y < end.y; y = y + 1u) {
            for (var x = first.x; x < end.x; x = x + 1u) {
                if buf_water[x + cells.x * (y + cells.y * z)] > 0.0 {
                    bits = bits | LB_LIQUID;
                }
            }
        }
    }
    for (var z = first.z; z <= end.z; z = z + 1u) {
        for (var y = first.y; y <= end.y; y = y + 1u) {
            for (var x = first.x; x <= end.x; x = x + 1u) {
                if buf_solid[x + nodes.x * (y + nodes.y * z)] < 0.0 {
                    bits = bits | LB_SOLID;
                }
            }
        }
    }
    let low = first * s;
    let high = end * s;
    var below = false;
    var above = false;
    for (var z = low.z; z <= high.z; z = z + 1u) {
        for (var y = low.y; y <= high.y; y = y + 1u) {
            let row = levels.x * (y + levels.y * z);
            for (var x = low.x; x <= high.x; x = x + 1u) {
                let inside = buf_level_set[row + x] < 0.0;
                below = below || inside;
                above = above || !inside;
            }
        }
    }
    if below && above {
        bits = bits | LB_SURFACE;
    }
    return bits;
}
