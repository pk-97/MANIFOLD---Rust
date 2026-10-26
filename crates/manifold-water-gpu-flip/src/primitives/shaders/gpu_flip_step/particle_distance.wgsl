// One thread per cell, the particles to `cell_out`: the signed distance at
// the cell's centre, each live particle a ball of radius r = √3·h/2, the
// engine's scatter box [floor((q − 2r − min) / h), floor((q + 2r − min) / h)]
// taken as a gather. Starts at 3h; reads the 27 cells around the cell, then
// the ring two out when φ is still over 1.5h − r. Even an empty first ring
// can have a contributing particle in ring two. A
// value within 0.005h of zero moves to ±0.005h by its sign, zero to −0.005h.
@compute @workgroup_size(256)
fn particle_distance(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = c_cell_index(gid.x);
    if idx == TS_NONE {
        return;
    }
    let n = ts_cells();
    let p = ts_cell_coord(idx);
    let h = u.cell_size;
    let centre = u.box_min + (vec3<f32>(p) + vec3<f32>(0.5)) * h;
    let radius = 0.8660254 * h;
    let search = 2.0 * radius;
    let slots = u.capacity;
    var distance = 3.0 * h;
    for (var ring = 1; ring <= 2; ring = ring + 1) {
        if ring == 2 && distance <= 1.5 * h - radius {
            break;
        }
        let first = max(p - vec3<i32>(ring), vec3<i32>(0));
        let last = min(p + vec3<i32>(ring), n - vec3<i32>(1));
        for (var z = first.z; z <= last.z; z = z + 1) {
            for (var y = first.y; y <= last.y; y = y + 1) {
                for (var x = first.x; x <= last.x; x = x + 1) {
                    let offset = abs(vec3<i32>(x, y, z) - p);
                    if ring == 2 && max(max(offset.x, offset.y), offset.z) < 2 {
                        continue;
                    }
                    let range = ranges[ts_cell_at(vec3<i32>(x, y, z))];
                    let start = min(range.start, slots);
                    let end = start + min(range.count, slots - start);
                    for (var s = start; s < end; s = s + 1u) {
                        let particle = sorted[s];
                        if !(particle.position_radius.w > 0.0) {
                            continue;
                        }
                        let q = particle.position_radius.xyz;
                        let low = vec3<i32>(floor((q - vec3<f32>(search) - u.box_min) / h));
                        let high = vec3<i32>(floor((q + vec3<f32>(search) - u.box_min) / h));
                        if any(p < low) || any(p > high) {
                            continue;
                        }
                        distance = min(distance, length(centre - q) - radius);
                    }
                }
            }
        }
    }
    let eps = 0.005 * h;
    if abs(distance) < eps {
        distance = select(-eps, eps, distance > 0.0);
    }
    cell_out[idx] = distance;
}
