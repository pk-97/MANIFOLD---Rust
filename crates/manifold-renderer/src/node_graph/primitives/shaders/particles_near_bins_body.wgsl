// node.particles_near_bins — fusable BUFFER body, GATHER. One thread per bin
// of a particle sort's grid: how many particles sit in the bins within
// `reach` bins of it, a (2·reach + 1)³ box clipped to the grid. Slots past
// the grid hold 0. `cell_ranges` (CellRange) is gathered through
// `buf_cell_ranges`.

fn body(idx: u32, count: u32, bins_x: i32, bins_y: i32, bins_z: i32, reach: i32) -> u32 {
    let bins = vec3<i32>(bins_x, bins_y, bins_z);
    if any(bins < vec3<i32>(1)) || idx >= u32(bins.x) * u32(bins.y) * u32(bins.z) {
        return 0u;
    }
    let bin = vec3<i32>(vec3<u32>(idx % u32(bins.x), (idx / u32(bins.x)) % u32(bins.y), idx / (u32(bins.x) * u32(bins.y))));
    let r = vec3<i32>(clamp(reach, 1, 4));
    let low = max(bin - r, vec3<i32>(0));
    let high = min(bin + r, bins - vec3<i32>(1));
    var total = 0u;
    for (var z = low.z; z <= high.z; z = z + 1) {
        for (var y = low.y; y <= high.y; y = y + 1) {
            for (var x = low.x; x <= high.x; x = x + 1) {
                total = total + buf_cell_ranges[u32(x + bins.x * (y + bins.y * z))].count;
            }
        }
    }
    return total;
}
