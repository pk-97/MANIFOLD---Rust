// Body poses for the GPU liquid atoms (LIQUID_SOLVER_SEAM_DESIGN.md section
// 3.6), declared through `wgsl_includes`. Matches liquid::bodies::body_pose_at.

// v turned by the unit quaternion q (xyzw).
fn liquid_rotate(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    let t = 2.0 * cross(q.xyz, v);
    return v + q.w * t + cross(q.xyz, t);
}

// q after turning at the constant world-frame angular velocity w for t
// seconds: d ⊗ q, d the turn by |w|·t about w.
fn liquid_turn(q: vec4<f32>, w: vec3<f32>, t: f32) -> vec4<f32> {
    let speed = length(w);
    let angle = speed * t;
    if !(angle > 0.0) {
        return q;
    }
    let d = vec4<f32>(w * (sin(0.5 * angle) / speed), cos(0.5 * angle));
    return vec4<f32>(
        d.w * q.x + d.x * q.w + d.y * q.z - d.z * q.y,
        d.w * q.y - d.x * q.z + d.y * q.w + d.z * q.x,
        d.w * q.z + d.x * q.y - d.y * q.x + d.z * q.w,
        d.w * q.w - d.x * q.x - d.y * q.y - d.z * q.z,
    );
}

// a × b with every product-and-sum an explicit fma. Fast math contracts a
// plain cross differently in a standalone and a fused kernel; a fixed form
// keeps fused buffer regions bit-exact (docs/FREEZE_COMPILER_MAP.md
// section 7 (precision contract)).
fn liquid_cross_fma(a: vec3<f32>, b: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        fma(a.y, b.z, -(a.z * b.y)),
        fma(a.z, b.x, -(a.x * b.z)),
        fma(a.x, b.y, -(a.y * b.x)),
    );
}

// The velocity of a body's material at world point x.
fn liquid_body_velocity(linear: vec3<f32>, angular: vec3<f32>, centre: vec3<f32>, x: vec3<f32>) -> vec3<f32> {
    return linear + liquid_cross_fma(angular, x - centre);
}

// A coupled body's support points (LIQUID_SOLVER_SEAM_DESIGN.md D16), five
// vec4 a point as liquid::bodies::BodySupports lays them: the lever arm and
// friction; the normal out of the support (zero ends the points) and the
// patch; the support's velocity and its spin about the normal; the patch
// centre's lever; the support's velocity there. The points of one Box3D
// manifold make a patch. The constants match manifold_physics::coupled_motion.
const LIQUID_SUPPORT_POINTS: u32 = 16u;
const LIQUID_SUPPORT_SWEEPS: u32 = 16u;
const LIQUID_HELD_SPEED: f32 = 1e-3;
const LIQUID_RANK_TOLERANCE: f32 = 1e-3;
const LIQUID_STICK_MARGIN: f32 = 0.999;

// A coupled body's pose and velocity inside its tick, and the support points
// it stays on: bit i of `closed` keeps point i closed; of `stuck`, point i's
// patch keeps its centre from sliding; of `unturned`, from turning about its
// normal.
struct LiquidBodyState {
    position: vec3<f32>,
    rotation: vec4<f32>,
    linear: vec3<f32>,
    angular: vec3<f32>,
    closed: u32,
    stuck: u32,
    unturned: u32,
};

// A body's inverse mass and world inverse inertia rows.
struct LiquidMobility {
    inv_mass: f32,
    x: vec3<f32>,
    y: vec3<f32>,
    z: vec3<f32>,
};

// A velocity increment and the supports it leaves the body on.
struct LiquidProjected {
    linear: vec3<f32>,
    angular: vec3<f32>,
    closed: u32,
    stuck: u32,
    unturned: u32,
};

// Points in `supports`: up to the first zero normal.
fn liquid_support_count(supports: ptr<function, array<vec4<f32>, 80>>) -> u32 {
    for (var p = 0u; p < LIQUID_SUPPORT_POINTS; p = p + 1u) {
        if all((*supports)[5u * p + 1u].xyz == vec3<f32>(0.0)) {
            return p;
        }
    }
    return LIQUID_SUPPORT_POINTS;
}

// The first point of point p's patch.
fn liquid_patch_lead(supports: ptr<function, array<vec4<f32>, 80>>, p: u32) -> u32 {
    let key = (*supports)[5u * p + 1u].w;
    for (var q = 0u; q < p; q = q + 1u) {
        if (*supports)[5u * q + 1u].w == key {
            return q;
        }
    }
    return p;
}

// n and two unit tangents completing it (Duff et al. 2017).
fn liquid_directions(n: vec3<f32>) -> array<vec3<f32>, 3> {
    let sign = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (sign + n.z);
    let b = n.x * n.y * a;
    return array<vec3<f32>, 3>(
        n,
        vec3<f32>(1.0 + sign * n.x * n.x * a, sign * b, -sign * n.x),
        vec3<f32>(b, sign + n.y * n.y * a, -n.y),
    );
}

fn liquid_inverse_inertia(m: LiquidMobility, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(m.x, v), dot(m.y, v), dot(m.z, v));
}

// The known increment (dv, dw) for a body moving at (v0, w0) with no motion
// into any support: Box3D's contact rule (Catto, sequential impulses)
// without restitution, softness or position correction. Patch by patch:
// each point's one-sided normal; twist friction about the normal inside
// μ·Σ(arm·λn); sliding friction at the patch centre inside the circle
// μ·Σλn. Twin of manifold_physics::coupled_motion's projection.
fn liquid_project_off(
    supports: ptr<function, array<vec4<f32>, 80>>,
    count: u32,
    m: LiquidMobility,
    v0: vec3<f32>,
    w0: vec3<f32>,
    dv_in: vec3<f32>,
    dw_in: vec3<f32>,
) -> LiquidProjected {
    var dv = dv_in;
    var dw = dw_in;
    var normal: array<f32, 16>;
    var slide: array<vec2<f32>, 16>;
    var twist: array<f32, 16>;
    var limits: array<vec2<f32>, 16>;
    for (var sweep = 0u; sweep < LIQUID_SUPPORT_SWEEPS; sweep = sweep + 1u) {
        for (var p = 0u; p < count; p = p + 1u) {
            if liquid_patch_lead(supports, p) != p {
                continue;
            }
            let friction = (*supports)[5u * p].w;
            let n = (*supports)[5u * p + 1u].xyz;
            let key = (*supports)[5u * p + 1u].w;
            let centre = (*supports)[5u * p + 3u].xyz;
            var total = 0.0;
            var twist_limit = 0.0;
            for (var q = p; q < count; q = q + 1u) {
                if (*supports)[5u * q + 1u].w != key {
                    continue;
                }
                let lever = (*supports)[5u * q].xyz;
                let arm = cross(lever, n);
                let turn = liquid_inverse_inertia(m, arm);
                let mass = m.inv_mass + dot(arm, turn);
                if mass > 0.0 {
                    let speed = dot(n, v0 + dv + cross(w0 + dw, lever) - (*supports)[5u * q + 2u].xyz);
                    let next = max(normal[q] - speed / mass, 0.0);
                    let change = next - normal[q];
                    normal[q] = next;
                    dv = dv + change * m.inv_mass * n;
                    dw = dw + change * turn;
                }
                total = total + normal[q];
                twist_limit = twist_limit + distance(lever, centre) * normal[q];
            }
            limits[p] = vec2<f32>(friction * total, friction * twist_limit);
            let spin = liquid_inverse_inertia(m, n);
            let spin_mass = dot(n, spin);
            if spin_mass > 0.0 {
                let speed = dot(n, w0 + dw) - (*supports)[5u * p + 2u].w;
                let next = clamp(twist[p] - speed / spin_mass, -limits[p].y, limits[p].y);
                let change = next - twist[p];
                twist[p] = next;
                dw = dw + change * spin;
            }
            var directions = liquid_directions(n);
            let t1 = directions[1];
            let t2 = directions[2];
            let a1 = cross(centre, t1);
            let a2 = cross(centre, t2);
            let r1 = liquid_inverse_inertia(m, a1);
            let r2 = liquid_inverse_inertia(m, a2);
            let k11 = m.inv_mass + dot(a1, r1);
            let k22 = m.inv_mass + dot(a2, r2);
            let k12 = dot(a1, r2);
            let det = k11 * k22 - k12 * k12;
            if det > 0.0 {
                let v = v0 + dv + cross(w0 + dw, centre) - (*supports)[5u * p + 4u].xyz;
                let s1 = dot(t1, v);
                let s2 = dot(t2, v);
                var next = slide[p] - vec2<f32>(k22 * s1 - k12 * s2, k11 * s2 - k12 * s1) / det;
                let size = length(next);
                if size > limits[p].x {
                    next = next * select(0.0, limits[p].x / size, size > 0.0);
                }
                let change = next - slide[p];
                slide[p] = next;
                dv = dv + m.inv_mass * (change.x * t1 + change.y * t2);
                dw = dw + change.x * r1 + change.y * r2;
            }
        }
    }
    var out: LiquidProjected;
    out.linear = dv;
    out.angular = dw;
    out.closed = 0u;
    out.stuck = 0u;
    out.unturned = 0u;
    for (var p = 0u; p < count; p = p + 1u) {
        let lever = (*supports)[5u * p].xyz;
        let n = (*supports)[5u * p + 1u].xyz;
        let arm = cross(lever, n);
        let mass = m.inv_mass + dot(arm, liquid_inverse_inertia(m, arm));
        let speed = dot(n, v0 + dv + cross(w0 + dw, lever) - (*supports)[5u * p + 2u].xyz);
        if mass > 0.0 && speed <= LIQUID_HELD_SPEED {
            out.closed = out.closed | (1u << p);
        }
        let lead = liquid_patch_lead(supports, p);
        let slide_limit = LIQUID_STICK_MARGIN * limits[lead].x;
        let twist_limit = LIQUID_STICK_MARGIN * limits[lead].y;
        if slide_limit > 0.0 && length(slide[lead]) < slide_limit {
            out.stuck = out.stuck | (1u << p);
        }
        if twist_limit > 0.0 && abs(twist[lead]) < twist_limit {
            out.unturned = out.unturned | (1u << p);
        }
    }
    return out;
}

// The coupled motion law (LIQUID_SOLVER_SEAM_DESIGN.md D15, D16), the twin of
// manifold_physics::coupled_motion::coupled_state_at: a dynamic body t
// seconds into its tick from its tick-start state, with push and turn the
// reaction impulse the liquid has put on it so far and h Box3D's substep.
// The supports first stop any motion into them at the tick's start; the
// known increment dv = a·t + push/m (dw likewise) then loses its motion into
// them; v = v0' + dv, x = x0 + v0'·t + ½·dv·(t + h); angular the same form as
// a world rotation vector.
fn liquid_body_state(
    position: vec3<f32>,
    rotation: vec4<f32>,
    linear: vec3<f32>,
    angular: vec3<f32>,
    m: LiquidMobility,
    acceleration: vec3<f32>,
    angular_acceleration: vec3<f32>,
    supports: ptr<function, array<vec4<f32>, 80>>,
    push: vec3<f32>,
    turn: vec3<f32>,
    t: f32,
    h: f32,
) -> LiquidBodyState {
    let lead = 0.5 * (t + h);
    let count = liquid_support_count(supports);
    let stopped = liquid_project_off(supports, count, m, linear, angular, vec3<f32>(0.0), vec3<f32>(0.0));
    let v0 = linear + stopped.linear;
    let w0 = angular + stopped.angular;
    let known = liquid_project_off(
        supports, count, m, v0, w0,
        acceleration * t + m.inv_mass * push,
        angular_acceleration * t + liquid_inverse_inertia(m, turn),
    );
    var state: LiquidBodyState;
    state.position = position + v0 * t + known.linear * lead;
    state.rotation = liquid_turn(rotation, w0 * t + known.angular * lead, 1.0);
    state.linear = v0 + known.linear;
    state.angular = w0 + known.angular;
    state.closed = known.closed;
    state.stuck = known.stuck;
    state.unturned = known.unturned;
    return state;
}

// Where entry (i, j) of a packed 6 × 6 mobility sits: the upper triangle by
// rows, as manifold_physics::coupled_motion::mobility_index.
fn liquid_mobility_index(i: u32, j: u32) -> u32 {
    let lo = min(i, j);
    let hi = max(i, j);
    return lo * (13u - lo) / 2u + hi - lo;
}

// A packed 6 × 6 mobility: entry k (liquid_mobility_index) in component k % 4
// of field k / 4.
struct LiquidPackedMobility {
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    m3: vec4<f32>,
    m4: vec4<f32>,
    m5: vec4<f32>,
};

// The body's response to the liquid's pressure while it stays on its held
// supports, packed 21 floats in six vec4; twin of
// manifold_physics::coupled_motion::constrained_mobility. With M⁻¹ = L·Lᵀ
// and P the projector off the held rows whitened by Lᵀ (modified
// Gram–Schmidt, twice), it is (L·P)·(L·P)ᵀ: symmetric and positive
// semidefinite however it rounds. Nothing held gives M⁻¹.
fn liquid_constrained_mobility(
    supports: ptr<function, array<vec4<f32>, 80>>,
    count: u32,
    closed: u32,
    stuck: u32,
    unturned: u32,
    m: LiquidMobility,
) -> LiquidPackedMobility {
    var free: array<array<f32, 6>, 6>;
    for (var r = 0u; r < 3u; r = r + 1u) {
        free[r][r] = m.inv_mass;
        free[3u][3u + r] = m.x[r];
        free[4u][3u + r] = m.y[r];
        free[5u][3u + r] = m.z[r];
    }
    var l: array<array<f32, 6>, 6>;
    let root = sqrt(max(free[0][0], 0.0));
    for (var r = 0u; r < 3u; r = r + 1u) {
        l[r][r] = root;
    }
    let scale = abs(free[3][3]) + abs(free[4][4]) + abs(free[5][5]);
    for (var j = 3u; j < 6u; j = j + 1u) {
        var pivot = free[j][j];
        for (var k = 3u; k < j; k = k + 1u) {
            pivot = pivot - l[j][k] * l[j][k];
        }
        if pivot <= 1e-7 * scale {
            continue;
        }
        let diagonal = sqrt(pivot);
        l[j][j] = diagonal;
        for (var r = j + 1u; r < 6u; r = r + 1u) {
            var below = free[r][j];
            for (var k = 3u; k < j; k = k + 1u) {
                below = below - l[r][k] * l[j][k];
            }
            l[r][j] = below / diagonal;
        }
    }
    var basis: array<array<f32, 6>, 6>;
    var rank = 0u;
    for (var p = 0u; p < count; p = p + 1u) {
        let lever = (*supports)[5u * p].xyz;
        let n = (*supports)[5u * p + 1u].xyz;
        let centre = (*supports)[5u * p + 3u].xyz;
        let lead = liquid_patch_lead(supports, p) == p;
        let bit = 1u << p;
        var directions = liquid_directions(n);
        // The point's normal; then, once a patch, its two sliding rows at the
        // centre and its turn about the normal.
        for (var k = 0u; k < 4u; k = k + 1u) {
            var d = vec3<f32>(0.0);
            var arm = vec3<f32>(0.0);
            if k == 0u {
                if (closed & bit) == 0u {
                    continue;
                }
                d = n;
                arm = cross(lever, n);
            } else if k == 3u {
                if !lead || (unturned & bit) == 0u {
                    continue;
                }
                arm = n;
            } else {
                if !lead || (stuck & bit) == 0u {
                    continue;
                }
                d = directions[k];
                arm = cross(centre, d);
            }
            var row = array<f32, 6>(d.x, d.y, d.z, arm.x, arm.y, arm.z);
            var c: array<f32, 6>;
            var size = 0.0;
            for (var a = 0u; a < 6u; a = a + 1u) {
                var sum = 0.0;
                for (var b = 0u; b < 6u; b = b + 1u) {
                    sum = sum + l[b][a] * row[b];
                }
                c[a] = sum;
                size = size + sum * sum;
            }
            size = sqrt(size);
            if size <= 0.0 || rank == 6u {
                continue;
            }
            for (var repeat = 0u; repeat < 2u; repeat = repeat + 1u) {
                for (var q = 0u; q < rank; q = q + 1u) {
                    var along = 0.0;
                    for (var a = 0u; a < 6u; a = a + 1u) {
                        along = along + basis[q][a] * c[a];
                    }
                    for (var a = 0u; a < 6u; a = a + 1u) {
                        c[a] = c[a] - along * basis[q][a];
                    }
                }
            }
            var rest = 0.0;
            for (var a = 0u; a < 6u; a = a + 1u) {
                rest = rest + c[a] * c[a];
            }
            rest = sqrt(rest);
            if rest > LIQUID_RANK_TOLERANCE * size {
                for (var a = 0u; a < 6u; a = a + 1u) {
                    basis[rank][a] = c[a] / rest;
                }
                rank = rank + 1u;
            }
        }
    }
    var a: array<array<f32, 6>, 6>;
    if rank > 0u {
        for (var r = 0u; r < 6u; r = r + 1u) {
            for (var c = 0u; c < 6u; c = c + 1u) {
                var sum = 0.0;
                for (var k = 0u; k < 6u; k = k + 1u) {
                    var projector = select(0.0, 1.0, k == c);
                    for (var q = 0u; q < rank; q = q + 1u) {
                        projector = projector - basis[q][k] * basis[q][c];
                    }
                    sum = sum + l[r][k] * projector;
                }
                a[r][c] = sum;
            }
        }
    }
    var out: array<vec4<f32>, 6>;
    for (var r = 0u; r < 6u; r = r + 1u) {
        for (var c = r; c < 6u; c = c + 1u) {
            var value = free[r][c];
            if rank > 0u {
                value = 0.0;
                for (var k = 0u; k < 6u; k = k + 1u) {
                    value = value + a[r][k] * a[c][k];
                }
            }
            let index = liquid_mobility_index(r, c);
            out[index / 4u][index % 4u] = value;
        }
    }
    // A struct, not the array: Metal cannot assign a returned array.
    return LiquidPackedMobility(out[0], out[1], out[2], out[3], out[4], out[5]);
}
