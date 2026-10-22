// node.extrude_curve — fusable BUFFER body (freeze section 12, buffer domain),
// GATHER. Extrude a P-point outline curve (Array<CurvePoint>, x/y in curve
// space) along +Z into a positions+normals+uv grid, `cols` wide: `cols = P`
// normally, `P+1` when `close` duplicates the first outline point as the last
// column (a closed loop). `symmetric` centres the extrusion on the curve's
// plane (Blender's Extrude). The CPU mirror is
// manifold_node_engine::scene::curve_sweep::extrude_grid.
//
// Rows: steps+1 side rows, plus with `caps` a start cap (centre, rim) before
// them and an end cap (rim, centre) after. A cap fans from the curve's origin;
// the rim row it shares with the side is doubled so the edge is hard.
//
// Normals are exact, oriented as make_triangles' finite differences orient
// them (cross(along rows, along columns)): the side faces across the outline,
// a cap along Z. A closed outline faces out of the solid (see `reversed`
// below). Wire node.make_triangles with Keep Normals downstream.
//
// ABI: `outline` is a BUFFER GATHER input — the body indexes `buf_outline`
// directly. `outline_len` is the derived element count.
fn ec_point(i: i32) -> vec2<f32> {
    let pt = buf_outline[u32(i)];
    return vec2<f32>(pt.x, pt.y);
}

// The outline's direction at point i: a central difference, wrapping round a
// loop (close, or an end point repeating the start), one-sided at an open end.
fn ec_tangent(i: i32, p_len: i32, looped: bool) -> vec2<f32> {
    if p_len < 2 {
        return vec2<f32>(0.0, 0.0);
    }
    var prev = max(i - 1, 0);
    var next = min(i + 1, p_len - 1);
    if looped {
        var distinct = p_len;
        if ec_ends_meet(p_len) {
            distinct = p_len - 1;
        }
        let j = i % distinct;
        prev = (j + distinct - 1) % distinct;
        next = (j + 1) % distinct;
    }
    return ec_point(next) - ec_point(prev);
}

fn ec_ends_meet(p_len: i32) -> bool {
    if p_len <= 2 {
        return false;
    }
    let first = ec_point(0);
    let reach = 1e-5 * max(length(first), 1.0);
    return length(ec_point(p_len - 1) - first) <= reach;
}

fn ec_sign(v: f32) -> f32 {
    if v < 0.0 {
        return -1.0;
    }
    return 1.0;
}

// Twice the signed area the outline encloses (shoelace), positive when it
// turns counter-clockwise about +Z. Summed in the CPU mirror's order.
fn ec_area2(p_len: i32) -> f32 {
    var area = 0.0;
    for (var i = 0; i < p_len; i = i + 1) {
        let a = ec_point(i);
        let b = ec_point((i + 1) % p_len);
        area = area + (a.x * b.y - b.x * a.y);
    }
    return area;
}

fn body(idx: u32, count: u32, depth: f32, steps: i32, close: u32, symmetric: u32, caps: u32, outline_len: u32) -> Element2 {
    let p_len = max(i32(outline_len), 1);
    let is_closed = close != 0u;
    var cols = p_len;
    if is_closed {
        cols = p_len + 1;
    }
    var rows = steps + 1;
    if caps != 0u {
        rows = steps + 5;
    }
    let total = u32(cols * rows);
    if idx >= total {
        return Element2(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 0.0), vec2<f32>(0.0, 0.0), vec2<f32>(0.0, 0.0), vec4<f32>(0.0), vec4<f32>(1.0));
    }

    let col = i32(idx) % cols;
    let row = i32(idx) / cols;
    // The step along the depth, and the cap role: 0 side, 1 start, 2 end.
    var step = row;
    var cap = 0;
    var centre = false;
    if caps != 0u {
        if row <= 1 {
            step = 0;
            cap = 1;
            centre = row == 0;
        } else if row <= steps + 2 {
            step = row - 2;
        } else {
            step = steps;
            cap = 2;
            centre = row == steps + 4;
        }
    }

    // Read forwards, a counter-clockwise loop pushed along +Z (or a
    // clockwise one along -Z) faces in, so it is read backwards: winding and
    // normals then face out. An open outline keeps its order.
    let ends_meet = ec_ends_meet(p_len);
    let looped = is_closed || ends_meet;
    let reversed = looped && ec_area2(p_len) * ec_sign(depth) > 0.0;
    var outline_col = col % p_len;
    if reversed {
        outline_col = (cols - 1 - col) % p_len;
    }
    // An end point that only nearly meets the start (sin(TAU) is not 0)
    // reads the start itself, so the seam's two columns share exact
    // positions and node.smooth_normals welds them: no crack under Inflate.
    if ends_meet && !is_closed && outline_col == p_len - 1 {
        outline_col = 0;
    }
    let pt = ec_point(outline_col);
    var t = ec_tangent(outline_col, p_len, looped);
    if reversed {
        t = -t;
    }

    let row_denom = max(f32(steps), 1.0);
    let col_denom = max(f32(cols - 1), 1.0);
    var shift = 0.0;
    if symmetric != 0u {
        shift = depth * 0.5;
    }
    let z = depth * f32(step) / row_denom - shift;

    var pos = vec3<f32>(pt.x, pt.y, z);
    let around = ec_sign(pt.x * t.y - pt.y * t.x);
    var normal = vec3<f32>(0.0, 0.0, around);
    if cap == 0 {
        let s = ec_sign(depth);
        let side = vec3<f32>(-s * t.y, s * t.x, 0.0);
        normal = vec3<f32>(0.0, 1.0, 0.0);
        if length(side) >= 1e-8 {
            normal = normalize(side);
        }
    } else {
        if cap == 2 {
            normal = -normal;
        }
        if centre {
            pos = vec3<f32>(0.0, 0.0, z);
        }
    }
    let uv = vec2<f32>(f32(col) / col_denom, f32(step) / row_denom);

    return Element2(pos, normal, uv, vec2<f32>(0.0, 0.0), vec4<f32>(0.0), vec4<f32>(1.0));
}
