// node.extend_lattice — fusable BUFFER body, GATHER. One thread per
// whitewater cell, one layer of FLIP's extrapolation (gridutils.h,
// extrapolateGridWithObserver): a known cell keeps its value; an unknown cell
// off the border with a known face neighbour off the border becomes known,
// holding the mean over its face neighbours that are known or on the border
// (FLIP counts border cells as done and never extrapolates them). Reads
// only the input layer, so passes chain. `values` is gathered; a grid past
// its array gives unknown.
//
// Ported from FLIP Fluids gridutils.h (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> Element {
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) {
        return Element(0.0, 0.0);
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if idx >= total || total > arrayLength(&buf_values) {
        return Element(0.0, 0.0);
    }
    let own = buf_values[idx];
    let c = ww_cell(idx, cells);
    if own.known > 0.0 || ww_on_border(c, cells) {
        return own;
    }
    var sum = 0.0;
    var hits = 0.0;
    var reached = false;
    for (var face = 0u; face < 6u; face = face + 1u) {
        let n = vec3<u32>(vec3<i32>(c) + ww_face_step(face));
        let other = buf_values[ww_cell_index(n, cells)];
        let border = ww_on_border(n, cells);
        if other.known > 0.0 && !border {
            reached = true;
        }
        if other.known > 0.0 || border {
            sum = sum + other.value;
            hits = hits + 1.0;
        }
    }
    if !reached {
        return own;
    }
    return Element(sum / hits, 1.0);
}
