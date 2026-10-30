// Mesh BVH queries for colliders: a line-for-line f32 port of MeshBvh::nearest
// and MeshBvh::winding in manifold-physics. The including body provides the
// buffer reads, since fused codegen renames gathered globals only in body text
// and this library declares no buffers:
//   fn cb_node(i: u32) -> vec4<f32>      vec4 i of the nodes buffer (4 per node:
//     [min.xyz, bitcast(first)], [max.xyz, bitcast(count)],
//     [dipole.xyz, radius], [centroid.xyz, 0])
//   fn cb_triangle(i: u32) -> vec4<f32>  vec4 i of the triangles buffer
//     (3 per triangle: a, b, c)
// A shape's tree is addressed by node_base (its root, in nodes) and
// triangle_base (its first triangle, in triangles). An inner node's `first` is
// relative to node_base and a leaf's relative to triangle_base, so per-geometry
// trees can be concatenated. Everything is in the shape's local units.

// Traversal stack entries. The build keeps depth <= 64 with the root at 1, so
// an inner node popped at level L (<= 63) leaves at most L - 1 siblings on the
// stack, and its two children bring that to L + 1 <= 64.
const CB_STACK: u32 = 64u;
// A node is far when |centroid - p| > CB_BETA * radius.
const CB_BETA: f32 = 2.0;
const CB_FOUR_PI: f32 = 12.566370614359172;

fn cb_box_distance_squared(p: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>) -> f32 {
    let d = max(max(lo - p, p - hi), vec3<f32>(0.0));
    return dot(d, d);
}

// Squared distance from p to a triangle (Ericson, Real-Time Collision
// Detection, section 5.1.5), region tests in the same order as the CPU.
fn cb_triangle_distance_squared(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>, c: vec3<f32>) -> f32 {
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return dot(p - a, p - a);
    }
    let bp = p - b;
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return dot(bp, bp);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let q = p - (a + ab * (d1 / (d1 - d3)));
        return dot(q, q);
    }
    let cp = p - c;
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return dot(cp, cp);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let q = p - (a + ac * (d2 / (d2 - d6)));
        return dot(q, q);
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let q = p - (b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6))));
        return dot(q, q);
    }
    let inv = 1.0 / (va + vb + vc);
    let q = p - (a + ab * (vb * inv) + ac * (vc * inv));
    return dot(q, q);
}

// Solid angle the triangle subtends at p, signed by its winding (Van
// Oosterom and Strackee 1983).
fn cb_signed_solid_angle(p: vec3<f32>, a0: vec3<f32>, b0: vec3<f32>, c0: vec3<f32>) -> f32 {
    let a = a0 - p;
    let b = b0 - p;
    let c = c0 - p;
    let la = length(a);
    let lb = length(b);
    let lc = length(c);
    let numerator = dot(a, cross(b, c));
    let denominator = la * lb * lc + dot(a, b) * lc + dot(b, c) * la + dot(c, a) * lb;
    return 2.0 * atan2(numerator, denominator);
}

// Unsigned distance from p to the nearest triangle, or cap when none is
// nearer. A node whose box is at least the best distance so far away is
// skipped, and the nearer child is popped first.
fn cb_nearest(node_base: u32, triangle_base: u32, p: vec3<f32>, cap: f32) -> f32 {
    var stack: array<u32, 64>;
    var best2 = cap * cap;
    var hit = false;
    var top = 1u;
    stack[0] = 0u;
    while top > 0u {
        top = top - 1u;
        let at = (node_base + stack[top]) * 4u;
        let lo = cb_node(at);
        let hi = cb_node(at + 1u);
        if cb_box_distance_squared(p, lo.xyz, hi.xyz) >= best2 {
            continue;
        }
        let first = bitcast<u32>(lo.w);
        let count = bitcast<u32>(hi.w);
        if count > 0u {
            for (var k = 0u; k < count; k = k + 1u) {
                let t = (triangle_base + first + k) * 3u;
                let d2 = cb_triangle_distance_squared(
                    p, cb_triangle(t).xyz, cb_triangle(t + 1u).xyz, cb_triangle(t + 2u).xyz,
                );
                if d2 < best2 {
                    best2 = d2;
                    hit = true;
                }
            }
        } else if top + 1u < CB_STACK {
            let l = (node_base + first) * 4u;
            let r = l + 4u;
            let dl = cb_box_distance_squared(p, cb_node(l).xyz, cb_node(l + 1u).xyz);
            let dr = cb_box_distance_squared(p, cb_node(r).xyz, cb_node(r + 1u).xyz);
            let left_nearer = dl <= dr;
            stack[top] = select(first, first + 1u, left_nearer);
            stack[top + 1u] = select(first + 1u, first, left_nearer);
            top = top + 2u;
        }
    }
    return select(cap, sqrt(best2), hit);
}

// Generalized winding number at p: about 1 inside an outward-wound closed
// surface, -1 inside an inside-out one, 0 outside, fractional near holes. A far
// node adds its dipole term; a near leaf adds each triangle's exact solid
// angle. The far test applies to leaves too, as on the CPU.
fn cb_winding(node_base: u32, triangle_base: u32, p: vec3<f32>) -> f32 {
    var stack: array<u32, 64>;
    var sum = 0.0;
    var top = 1u;
    stack[0] = 0u;
    while top > 0u {
        top = top - 1u;
        let at = (node_base + stack[top]) * 4u;
        let head = cb_node(at);
        let tail = cb_node(at + 1u);
        let dipole_radius = cb_node(at + 2u);
        let d = cb_node(at + 3u).xyz - p;
        let r2 = dot(d, d);
        let reach = CB_BETA * dipole_radius.w;
        let first = bitcast<u32>(head.w);
        let count = bitcast<u32>(tail.w);
        if r2 > reach * reach {
            sum = sum + dot(d, dipole_radius.xyz) / (CB_FOUR_PI * r2 * sqrt(r2));
        } else if count > 0u {
            for (var k = 0u; k < count; k = k + 1u) {
                let t = (triangle_base + first + k) * 3u;
                sum = sum + cb_signed_solid_angle(
                    p, cb_triangle(t).xyz, cb_triangle(t + 1u).xyz, cb_triangle(t + 2u).xyz,
                ) / CB_FOUR_PI;
            }
        } else if top + 1u < CB_STACK {
            stack[top] = first + 1u;
            stack[top + 1u] = first;
            top = top + 2u;
        }
    }
    return sum;
}

// Signed distance by the collider rule: the nearest-triangle distance d
// (searched up to band) and winding number w. Where |w| is within a quarter of
// a half (beside a hole or open sheet) the point is a shell, d - shell, so an
// open surface is `shell` thick each side; otherwise inside (|w| > 1/2) is -d
// and outside d. Results saturate at +-band (band - shell for a shell). Past
// the band round the root's box no triangle is within reach: the distance is
// the band there, with no winding walk.
fn cb_phi(node_base: u32, triangle_base: u32, p: vec3<f32>, band: f32, shell: f32) -> f32 {
    let root = 4u * node_base;
    if any(p < cb_node(root).xyz - vec3<f32>(band)) || any(p > cb_node(root + 1u).xyz + vec3<f32>(band)) {
        return band;
    }
    let d = cb_nearest(node_base, triangle_base, p, band);
    let w = cb_winding(node_base, triangle_base, p);
    if abs(abs(w) - 0.5) < 0.25 {
        return d - shell;
    }
    if abs(w) > 0.5 {
        return -d;
    }
    return d;
}
