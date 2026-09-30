// node.nearest_crossing — fusable BUFFER body, GATHER. One thread per
// whitewater cell: of the cell's own crossing and its 26 neighbours', the one
// nearest the cell centre (the own one on a tie, then z, y, x order); level
// is the cell's own. Reads only the input layer, so passes chain. `crossings`
// is gathered; a grid past its array gives no crossing and level 1e6.

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> Element {
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return Element(vec3<f32>(1e6), 1e6);
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if idx >= total || total > arrayLength(&buf_crossings) {
        return Element(vec3<f32>(1e6), 1e6);
    }
    let c = ww_cell(idx, cells);
    let own = buf_crossings[idx];
    let centre = vec3<f32>(c) + vec3<f32>(0.5);
    var best = own.crossing;
    let d = best - centre;
    var nearest = dot(d, d);
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                let n = vec3<i32>(c) + vec3<i32>(dx, dy, dz);
                if (dx == 0 && dy == 0 && dz == 0) || any(n < vec3<i32>(0)) || any(n >= vec3<i32>(cells)) {
                    continue;
                }
                let other = buf_crossings[ww_cell_index(vec3<u32>(n), cells)].crossing;
                let e = other - centre;
                let ee = dot(e, e);
                if ee < nearest {
                    nearest = ee;
                    best = other;
                }
            }
        }
    }
    return Element(best, own.level);
}
