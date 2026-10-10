// node.advect_whitewater — fusable BUFFER body, COINCIDENT pool, GATHER faces
// and solid. One FLIP tick of a live whitewater particle by its type
// (diffuseparticlesimulation.cpp:2250-2600), every limit behaviour collide:
//   spray: gravity and per-id drag, then the collision march; a collision
//     with a usable normal sets velocity from the old one split along it
//     (friction on the tangent part, restitution on the normal part) plus
//     gravity;
//   bubble: buoyancy against gravity and drag toward the liquid velocity;
//   foam: the liquid velocity times the advection strength;
//   all: a particle that moved faster than 1.1 times its new speed dies
//     (lifetime -1e6), and so does one whose travel is not finite.
// Work is in FLIP's local frame: metres from the grid's first node. The
// liquid velocity is FLIP's MAC trilinear at the old position (0 outside
// the grid), the solid its node lattice read trilinearly (a node past the
// lattice reads 0). FLIP's near-solid early-out is dropped
// (GPU_WHITEWATER_DESIGN.md section 3.9); its range check stays. Slots with
// kind 3 is empty; kind 4 is dust, with FLIP's ID-dependent buoyancy and drag.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// FLIP's boundary box: 3 cells and 1e-6 m in from the domain, then
// _solidBufferWidth (a quarter cell) more, each side moving by half.
const AW_BOX_INSET: f32 = 1.625;
const AW_BOX_EPSILON: f32 = 0.5e-6;
// _solidBufferWidth, _diffuseParticleStepDistanceFactor and the CFL number,
// cells; _maxVelocityFactor.
const AW_SOLID_BUFFER: f32 = 0.25;
const AW_STEP: f32 = 0.5;
const AW_MAX_RESOLVE: f32 = 5.0;
const AW_MAX_VELOCITY: f32 = 1.1;
// _nearSolidGridCellSizeFactor, cells.
const AW_NEAR_SOLID: f32 = 3.0;
const AW_EPS: f32 = 1e-6;
const AW_DEAD: f32 = -1e6;
// _diffuseParticleIDLimit - 1.
const AW_ID_TOP: f32 = 255.0;

fn aw_face_len(axis: u32) -> u32 {
    if axis == 0u {
        return arrayLength(&buf_face_u);
    }
    if axis == 1u {
        return arrayLength(&buf_face_v);
    }
    return arrayLength(&buf_face_w);
}

fn aw_face(axis: u32, i: u32, step: u32, cells: vec3<u32>) -> f32 {
    if step != 0xffffffffu {
        var dims = cells; dims[axis] = dims[axis] + 1u;
        let at = step * dims.x * dims.y * dims.z + i;
        if axis == 0u { return buf_substep_u[at]; }
        if axis == 1u { return buf_substep_v[at]; }
        return buf_substep_w[at];
    }
    if axis == 0u {
        return buf_face_u[i];
    }
    if axis == 1u {
        return buf_face_v[i];
    }
    return buf_face_w[i];
}

// FLIP's MAC trilinear at grid position q.
fn aw_velocity(q: vec3<f32>, cells: vec3<u32>, face_cells: vec3<u32>, step: u32) -> vec3<f32> {
    if any(q < vec3<f32>(0.0)) || any(q >= vec3<f32>(cells)) {
        return vec3<f32>(0.0);
    }
    let pad = lf_pad(cells, face_cells);
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        let len = aw_face_len(axis);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let i = lf_face_index(base + vec3<i32>(ww_corner(corner)), axis, pad, face_cells);
            if i != LF_NONE && i < len {
                sum = sum + ww_corner_weight(f, corner) * aw_face(axis, i, step, face_cells);
            }
        }
        v[axis] = sum;
    }
    return v;
}

fn aw_node(n: vec3<i32>, nodes: vec3<u32>) -> f32 {
    if any(n < vec3<i32>(0)) || any(n >= vec3<i32>(nodes)) {
        return 0.0;
    }
    let u = vec3<u32>(n);
    return buf_solid[u.x + nodes.x * (u.y + nodes.y * u.z)];
}

// The solid's distance at grid position q, metres.
fn aw_solid(q: vec3<f32>, nodes: vec3<u32>) -> f32 {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * aw_node(base + vec3<i32>(ww_corner(corner)), nodes);
    }
    return d;
}

// FLIP's trilinear gradient (interpolation.cpp:197), unscaled; corner index
// x + 2y + 4z.
fn aw_gradient(q: vec3<f32>, nodes: vec3<u32>) -> vec3<f32> {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var phi: array<f32, 8>;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        phi[corner] = aw_node(base + vec3<i32>(ww_corner(corner)), nodes);
    }
    let gx = mix(mix(phi[1] - phi[0], phi[3] - phi[2], f.y), mix(phi[5] - phi[4], phi[7] - phi[6], f.y), f.z);
    let gy = mix(mix(phi[2] - phi[0], phi[3] - phi[1], f.x), mix(phi[6] - phi[4], phi[7] - phi[5], f.x), f.z);
    let gz = mix(mix(phi[4] - phi[0], phi[5] - phi[1], f.x), mix(phi[6] - phi[2], phi[7] - phi[3], f.x), f.y);
    return vec3<f32>(gx, gy, gz);
}

fn aw_inside(p: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>) -> bool {
    return all(p >= lo) && all(p < hi);
}

// Whether a local position's near-solid cell lies in FLIP's near-solid grid.
fn aw_in_near_solid(p: vec3<f32>, h: f32, cells: vec3<u32>) -> bool {
    let g = floor(p / (AW_NEAR_SOLID * h));
    let top = vec3<f32>((cells + vec3<u32>(2u)) / vec3<u32>(3u));
    return all(g >= vec3<f32>(0.0)) && all(g < top);
}

fn aw_step(
    idx: u32,
    count: u32,
    e_pool: Element,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    dt: f32,
    foam_advection: f32,
    bubble_buoyancy: f32,
    bubble_drag: f32,
    spray_drag: f32,
    spray_drag_variance: f32,
    spray_restitution: f32,
    spray_friction: f32,
    face_step: u32,
) -> Element {
    var out = e_pool;
    if (e_pool.kind == 3u || e_pool.kind > 4u) || !(dt > 0.0) {
        return out;
    }
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    let face_cells = vec3<u32>(max(round(vec3<f32>(face_cells_x, face_cells_y, face_cells_z)), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) || any(face_cells == vec3<u32>(0u)) || nodes.x * nodes.y * nodes.z > arrayLength(&buf_solid) {
        return out;
    }
    let cells = nodes - vec3<u32>(1u);
    if any(face_cells > cells) {
        return out;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let origin = vec3<f32>(center_x, center_y, center_z) - 0.5 * size;
    let p = e_pool.position_lifetime.xyz - origin;
    let v = e_pool.velocity;
    let gravity = vec3<f32>(gravity_x, gravity_y, gravity_z);
    let lo = vec3<f32>(AW_BOX_INSET * h + AW_BOX_EPSILON);
    let hi = vec3<f32>(cells) * h - lo;
    let spray = e_pool.kind == 2u;

    var nextv: vec3<f32>;
    if spray {
        let factor = f32(e_pool.id) / AW_ID_TOP;
        let mind = max(spray_drag - spray_drag * spray_drag_variance, 0.0);
        let maxd = spray_drag + spray_drag * spray_drag_variance;
        let drag = mind + (1.0 - factor) * (maxd - mind);
        nextv = v + gravity * dt + (-drag * v * dt);
    } else {
        let vmac = aw_velocity(p / h, cells, face_cells, face_step);
        if e_pool.kind == 4u {
            let factor = f32(e_pool.id) / AW_ID_TOP;
            let buoyancy = -2.0 + factor * (-6.0 + 2.0);
            let drag = 0.375 + (1.0 - factor) * (0.625 - 0.375);
            nextv = v + dt * (-buoyancy * gravity + drag * (vmac - v) / dt);
        } else if e_pool.kind == 0u {
            nextv = v + dt * (-bubble_buoyancy * gravity + bubble_drag * (vmac - v) / dt);
        } else {
            nextv = foam_advection * vmac;
        }
    }
    let nextp = p + nextv * dt;

    let travel = length(nextp - p);
    // By its bits: fast math may fold a NaN comparison away.
    if (bitcast<u32>(travel) & 0x7f800000u) == 0x7f800000u {
        out.position_lifetime.w = AW_DEAD;
        return out;
    }
    var resolved = nextp;
    var bounced = false;
    var bounce = vec3<f32>(0.0);
    if aw_in_near_solid(p, h, cells) && aw_in_near_solid(nextp, h, cells) && travel >= AW_EPS {
        let step = AW_STEP * h;
        let steps = i32(ceil(travel / step));
        let dir = (nextp - p) / travel;
        var last = p;
        var current = p;
        var found = false;
        var hit_phi = 0.0;
        for (var i = 0; i < steps; i = i + 1) {
            if i == steps - 1 {
                current = nextp;
            } else {
                current = p + f32(i + 1) * step * dir;
            }
            let phi = aw_solid(current / h, nodes);
            if phi < 0.0 || !aw_inside(current, lo, hi) {
                hit_phi = phi;
                found = true;
                break;
            }
            last = current;
        }
        if found {
            let reach = AW_MAX_RESOLVE * h;
            let grad = aw_gradient(current / h, nodes);
            if length(grad) > AW_EPS {
                let n = normalize(grad);
                resolved = current - (hit_phi - AW_SOLID_BUFFER * h) * n;
                if aw_solid(resolved / h, nodes) < 0.0 || length(resolved - current) > reach {
                    resolved = last;
                }
                let u = dot(v, n) * n;
                bounce = (1.0 - spray_friction) * (v - u) - spray_restitution * u;
                bounced = true;
            } else {
                resolved = last;
            }
            if !aw_inside(resolved, lo, hi) {
                let before = resolved;
                resolved = min(max(resolved, lo), hi - vec3<f32>(AW_EPS));
                if aw_solid(resolved / h, nodes) < 0.0 || length(resolved - before) > reach {
                    resolved = last;
                }
            }
        }
    }
    if spray && bounced {
        nextv = bounce + gravity * dt;
    }
    if length(resolved - p) * (1.0 / dt) > AW_MAX_VELOCITY * length(nextv) {
        out.position_lifetime.w = AW_DEAD;
    }
    out.position_lifetime = vec4<f32>(resolved + origin, out.position_lifetime.w);
    out.velocity = nextv;
    return out;
}

fn body(
    idx: u32,
    count: u32,
    e_pool: Element,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    dt: f32,
    foam_advection: f32,
    bubble_buoyancy: f32,
    bubble_drag: f32,
    spray_drag: f32,
    spray_drag_variance: f32,
    spray_restitution: f32,
    spray_friction: f32,
    substep_count: f32,
    field_nodes_x: f32,
    field_nodes_y: f32,
    field_nodes_z: f32,
    field_spacing: f32,
    force_lattices: f32,
    tick_index: f32,
    first_tick: f32,
) -> Element {
    let steps = u32(max(substep_count, 0.0));
    let origin = vec3<f32>(center_x,center_y,center_z) - 0.5 * vec3<f32>(size_x,size_y,size_z);
    let h = size_x / (nodes_x - 1.0);
    let padding = 0.5 * (nodes_x - 1.0 - face_cells_x);
    let field_origin = origin + vec3<f32>(padding * h);
    let dims = vec3<u32>(vec3<f32>(field_nodes_x,field_nodes_y,field_nodes_z));
    let stride = dims.x * dims.y * dims.z * 4u;
    let base = liquid_field_force_base(i32(tick_index), i32(first_tick), i32(force_lattices), dims);
    var particle = e_pool;
    for (var step = 0u; step < max(steps,1u); step = step + 1u) {
        var duration = dt;
        var face_step = 0xffffffffu;
        var event = 0u;
        if steps > 0u {
            duration = buf_substep_schedule[step * 4u];
            event = bitcast<u32>(buf_substep_schedule[step * 4u + 2u]);
            face_step = step;
        }
        if duration <= 0.0 { continue; }
        var acceleration = vec3<f32>(gravity_x,gravity_y,gravity_z);
        var impulse = vec3<f32>(0.0);
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let c = liquid_field_corner(particle.position_lifetime.xyz, field_origin, field_spacing, dims, corner);
            for (var a = 0u; a < 3u; a = a + 1u) {
                if force_lattices > 0.0 {
                    acceleration[a] = fma(buf_forces[base + c.index * 4u + a], c.weight, acceleration[a]);
                }
                if (event & 0x80000000u) != 0u {
                    let at = (event & 0x7fffffffu) * stride + c.index * 4u + a;
                    if at < arrayLength(&buf_impulses) { impulse[a] = fma(buf_impulses[at], c.weight, impulse[a]); }
                }
            }
        }
        // Foam gets the impulse through the already-forced liquid velocity.
        if particle.kind != 1u && (event & 0x80000000u) != 0u { particle.velocity = particle.velocity + impulse; }
        particle = aw_step(idx, count, particle, center_x, center_y, center_z, size_x, size_y, size_z, nodes_x, nodes_y, nodes_z, face_cells_x, face_cells_y, face_cells_z, acceleration.x, acceleration.y, acceleration.z, duration, foam_advection, bubble_buoyancy, bubble_drag, spray_drag, spray_drag_variance, spray_restitution, spray_friction, face_step);
    }
    return particle;
}
