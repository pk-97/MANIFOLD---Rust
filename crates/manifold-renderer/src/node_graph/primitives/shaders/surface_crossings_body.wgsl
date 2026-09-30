// node.surface_crossings — fusable BUFFER body, GATHER. One thread per
// whitewater cell. The cell's footprint on the refined level set holds
// (s+1)³ nodes and 3·s·(s+1)² edges. An edge whose ends straddle zero, with
// neither end in a solid, crosses the surface at the linear root; the
// crossing nearest the cell centre is kept, in grid cells from the grid's
// first node (1e6 on every axis when there is none). level is the level set
// at the cell centre, trilinear on the refined nodes. The solid at a refined
// node is the trilinear of the cell's eight solid corners. `level_set` and
// `solid` are gathered; a lattice past its array gives no crossing and
// level 1e6.

fn body(
    idx: u32,
    count: u32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    level_nodes_x: f32,
    level_nodes_y: f32,
    level_nodes_z: f32,
) -> Element {
    var out = Element(vec3<f32>(1e6), 1e6);
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    let levels = vec3<u32>(max(vec3<f32>(level_nodes_x, level_nodes_y, level_nodes_z), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) || any(levels < vec3<u32>(2u)) {
        return out;
    }
    let cells = nodes - vec3<u32>(1u);
    let s = (levels.x - 1u) / cells.x;
    if s < 1u || s > 4u || any(levels - vec3<u32>(1u) != cells * s)
        || idx >= cells.x * cells.y * cells.z
        || nodes.x * nodes.y * nodes.z > arrayLength(&buf_solid)
        || levels.x * levels.y * levels.z > arrayLength(&buf_level_set) {
        return out;
    }
    let c = ww_cell(idx, cells);
    var corners: array<f32, 8>;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let n = c + ww_corner(corner);
        corners[corner] = buf_solid[n.x + nodes.x * (n.y + nodes.y * n.z)];
    }
    // The footprint's level and whether each node is clear of solids.
    let side = s + 1u;
    let base = c * s;
    var phi: array<f32, 125>;
    var clear: array<bool, 125>;
    for (var z = 0u; z < side; z = z + 1u) {
        for (var y = 0u; y < side; y = y + 1u) {
            for (var x = 0u; x < side; x = x + 1u) {
                let slot = x + side * (y + side * z);
                let q = base + vec3<u32>(x, y, z);
                phi[slot] = buf_level_set[q.x + levels.x * (q.y + levels.y * q.z)];
                let f = vec3<f32>(f32(x), f32(y), f32(z)) / f32(s);
                var solid = 0.0;
                for (var corner = 0u; corner < 8u; corner = corner + 1u) {
                    solid = solid + ww_corner_weight(f, corner) * corners[corner];
                }
                clear[slot] = solid >= 0.0;
            }
        }
    }
    let centre = vec3<f32>(0.5 * f32(s));
    var best = 3.0e38;
    for (var a = 0u; a < 3u; a = a + 1u) {
        var step = vec3<u32>(0u);
        step[a] = 1u;
        var top = vec3<u32>(s);
        top[a] = s - 1u;
        for (var z = 0u; z <= top.z; z = z + 1u) {
            for (var y = 0u; y <= top.y; y = y + 1u) {
                for (var x = 0u; x <= top.x; x = x + 1u) {
                    let p0 = vec3<u32>(x, y, z);
                    let p1 = p0 + step;
                    let i0 = p0.x + side * (p0.y + side * p0.z);
                    let i1 = p1.x + side * (p1.y + side * p1.z);
                    let v0 = phi[i0];
                    let v1 = phi[i1];
                    if !clear[i0] || !clear[i1] || (v0 < 0.0) == (v1 < 0.0) {
                        continue;
                    }
                    let root = vec3<f32>(p0) + (v0 / (v0 - v1)) * vec3<f32>(step);
                    let d = root - centre;
                    let dd = dot(d, d);
                    if dd < best {
                        best = dd;
                        out.crossing = (vec3<f32>(base) + root) / f32(s);
                    }
                }
            }
        }
    }
    let low = min(vec3<u32>(floor(centre)), vec3<u32>(s - 1u));
    let f = centre - vec3<f32>(low);
    var level = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let q = low + ww_corner(corner);
        level = level + ww_corner_weight(f, corner) * phi[q.x + side * (q.y + side * q.z)];
    }
    out.level = level;
    return out;
}
