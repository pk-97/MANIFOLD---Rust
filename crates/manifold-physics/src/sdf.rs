//! The f64 triangle queries the mesh tree's CPU reference is made of:
//! point-to-triangle distance and solid angle.

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

/// Solid angle the triangle subtends at `p`, signed by its winding (Van
/// Oosterom and Strackee 1983).
pub(crate) fn signed_solid_angle(p: [f64; 3], [a, b, c]: &[[f64; 3]; 3]) -> f64 {
    let (a, b, c) = (sub(*a, p), sub(*b, p), sub(*c, p));
    let (la, lb, lc) = (length(a), length(b), length(c));
    let numerator = dot(a, cross(b, c));
    let denominator = la * lb * lc + dot(a, b) * lc + dot(b, c) * la + dot(c, a) * lb;
    2.0 * numerator.atan2(denominator)
}

/// Squared distance from `p` to the closest point of a triangle (Ericson,
/// Real-Time Collision Detection, section 5.1.5).
pub fn distance_squared_to_triangle(p: [f64; 3], [a, b, c]: &[[f64; 3]; 3]) -> f64 {
    let closest = |q: [f64; 3]| {
        let d = sub(p, q);
        dot(d, d)
    };
    let along = |from: [f64; 3], edge: [f64; 3], t: f64| -> [f64; 3] {
        std::array::from_fn(|axis| from[axis] + t * edge[axis])
    };
    let ab = sub(*b, *a);
    let ac = sub(*c, *a);
    let ap = sub(p, *a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return closest(*a);
    }
    let bp = sub(p, *b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return closest(*b);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return closest(along(*a, ab, d1 / (d1 - d3)));
    }
    let cp = sub(p, *c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return closest(*c);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return closest(along(*a, ac, d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return closest(along(*b, sub(*c, *b), (d4 - d3) / ((d4 - d3) + (d5 - d6))));
    }
    let denominator = 1.0 / (va + vb + vc);
    let v = vb * denominator;
    let w = vc * denominator;
    closest(std::array::from_fn(|axis| {
        a[axis] + ab[axis] * v + ac[axis] * w
    }))
}
