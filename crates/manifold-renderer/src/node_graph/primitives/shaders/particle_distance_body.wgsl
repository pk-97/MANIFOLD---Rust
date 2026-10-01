// node.particle_distance — fusable BUFFER body, GATHER. One thread per
// lattice cell c: the signed distance at its centre from the particles,
// each a ball of radius r = √3·h/2 (half a cell's diagonal), read through
// the sort's cell ranges over the 5 × 5 × 5 cells around c. The engine
// scatters a particle at q into the cells whose index along each axis lies
// in [floor((q − 2r − min) / h), floor((q + 2r − min) / h)]; 2r < 2h, so
// that box never reaches past two cells of q's own, and the gather keeps a
// particle only when c is inside its box. Starts at 3h, takes the min of
// |centre − position| − r over those live particles (radius > 0) — the
// engine's field, except that a cell with no live particle in the 27 cells
// around it stays 3h where the engine can read down to 1.5h − r — then
// moves a value within 0.005h of zero to ±0.005h by its sign, zero to
// −0.005h. `sorted` (FluidParticle → Element) and `cell_ranges` (CellRange →
// Element2) are gathered; a lattice larger than `cell_ranges` gives zeros
// and a range past `sorted` is cut short. Cells past the lattice give 0.
//
// Ported from FLIP Fluids particlelevelset.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md

fn body(
    idx: u32,
    count: u32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
) -> f32 {
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    if idx >= cells || cells > arrayLength(&buf_cell_ranges) {
        return 0.0;
    }
    let p = vec3<i32>(
        i32(idx % u32(n.x)),
        i32((idx / u32(n.x)) % u32(n.y)),
        i32(idx / (u32(n.x) * u32(n.y))),
    );
    let lattice_min = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z);
    let centre = lattice_min + (vec3<f32>(p) + vec3<f32>(0.5)) * cell_size;
    let radius = 0.8660254 * cell_size;
    let search = 2.0 * radius;
    let particles = arrayLength(&buf_sorted);
    var phi = 3.0 * cell_size;
    var near = false;
    // The 27 bins first, then the ring two bins out. A particle in that ring
    // lies at least 1.5h from the centre along one axis, so the ring cannot
    // lower a φ at or below 1.5h − r. A cell with no live particle in its 27
    // bins skips the ring and stays 3h: the free surface reads φ only at
    // water cells and their neighbours, which always have one.
    for (var ring = 1; ring <= 2; ring = ring + 1) {
        if ring == 2 && (!near || phi <= 1.5 * cell_size - radius) {
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
                    let range = buf_cell_ranges[u32(x + n.x * (y + n.y * z))];
                    let start = min(range.start, particles);
                    let end = start + min(range.count, particles - start);
                    for (var s = start; s < end; s = s + 1u) {
                        let particle = buf_sorted[s];
                        if !(particle.position_radius.w > 0.0) {
                            continue;
                        }
                        near = true;
                        let q = particle.position_radius.xyz;
                        let low = vec3<i32>(floor((q - vec3<f32>(search) - lattice_min) / cell_size));
                        let high = vec3<i32>(floor((q + vec3<f32>(search) - lattice_min) / cell_size));
                        if any(p < low) || any(p > high) {
                            continue;
                        }
                        phi = min(phi, length(centre - q) - radius);
                    }
                }
            }
        }
    }
    let eps = 0.005 * cell_size;
    if abs(phi) < eps {
        phi = select(-eps, eps, phi > 0.0);
    }
    return phi;
}
