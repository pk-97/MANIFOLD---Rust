var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.advect_whitewater — fusable BUFFER body, COINCIDENT pool, GATHER faces,
// solid, the substep history, the force lattices and the world-tile table.
// One FLIP tick of a live whitewater particle by its type
// (diffuseparticlesimulation.cpp:2250-2600), every limit behaviour collide:
//   spray: gravity and per-id drag, then the collision march; a collision
//     with a usable normal sets velocity from the old one split along it
//     (friction on the tangent part, restitution on the normal part) plus
//     gravity;
//   bubble: buoyancy against gravity and drag toward the liquid velocity;
//   foam: the liquid velocity times the advection strength;
//   all: a particle that moved faster than 1.1 times its new speed dies
//     (lifetime -1e6), and so does one whose travel is not finite.
// Work is in world metres, grid positions from `ww_cell_position`. The
// liquid velocity is FLIP's MAC trilinear at the old position over the
// step's pooled faces (the face of cell f along an axis is the record of
// cell f, 0 where it has no weight or its tile is not held) or, under a
// schedule, over substep k's row of the history, which lies on the same
// table. The solid is the node array read trilinearly, a node in a tile the
// table does not hold reading the canonical element. FLIP's near-solid
// early-out is dropped, and so is the boundary box: a wall is a solid. Slots
// with kind 3 is empty; kind 4 is dust, with FLIP's ID-dependent buoyancy and drag.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// _solidBufferWidth, _diffuseParticleStepDistanceFactor and the CFL number,
// cells; _maxVelocityFactor.
const AW_SOLID_BUFFER: f32 = 0.25;
const AW_STEP: f32 = 0.5;
const AW_MAX_RESOLVE: f32 = 5.0;
const AW_MAX_VELOCITY: f32 = 1.1;
const AW_EPS: f32 = 1e-6;
const AW_DEAD: f32 = -1e6;
// _diffuseParticleIDLimit - 1.
const AW_ID_TOP: f32 = 255.0;
// The solid reads 1e30 in a tile no slot holds; stopping a node read here
// keeps the gradient's differences finite.
const AW_FAR: f32 = 1e4;

// Component `axis` of the face of cell c: the step's own faces, or substep
// `step` of the history (0xffffffff reads the step's own).
fn aw_face(axis: u32, c: vec3<i32>, step: u32) -> f32 {
    let i = ts_face(c);
    if step != 0xffffffffu {
        return buf_substep_faces[(step * ts_pool_len() + i) * 4u + axis];
    }
    let s = buf_faces[i];
    return lf_face_velocity(s.face_velocity, s.face_weight, axis);
}

// FLIP's MAC trilinear at grid position q.
fn aw_velocity(q: vec3<f32>, step: u32) -> vec3<f32> {
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            sum = sum + ww_corner_weight(f, corner) * aw_face(axis, base + vec3<i32>(ww_corner(corner)), step);
        }
        v[axis] = sum;
    }
    return v;
}

fn aw_node(n: vec3<i32>) -> f32 {
    return min(buf_solid[ts_cell_at(n)], AW_FAR);
}

// The solid's distance at grid position q, metres.
fn aw_solid(q: vec3<f32>) -> f32 {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * aw_node(base + vec3<i32>(ww_corner(corner)));
    }
    return d;
}

// FLIP's trilinear gradient (interpolation.cpp:197), unscaled; corner index
// x + 2y + 4z.
fn aw_gradient(q: vec3<f32>) -> vec3<f32> {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var phi: array<f32, 8>;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        phi[corner] = aw_node(base + vec3<i32>(ww_corner(corner)));
    }
    let gx = mix(mix(phi[1] - phi[0], phi[3] - phi[2], f.y), mix(phi[5] - phi[4], phi[7] - phi[6], f.y), f.z);
    let gy = mix(mix(phi[2] - phi[0], phi[3] - phi[1], f.x), mix(phi[6] - phi[4], phi[7] - phi[5], f.x), f.z);
    let gz = mix(mix(phi[4] - phi[0], phi[5] - phi[1], f.x), mix(phi[6] - phi[2], phi[7] - phi[3], f.x), f.y);
    return vec3<f32>(gx, gy, gz);
}

fn aw_step(
    e_pool: Element,
    cell_size: f32,
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
    let h = cell_size;
    let p = e_pool.position_lifetime.xyz;
    let v = e_pool.velocity;
    let gravity = vec3<f32>(gravity_x, gravity_y, gravity_z);
    let spray = e_pool.kind == 2u;
    let q = ww_cell_position(p, h);
    ts_home(ts_cell_at(vec3<i32>(floor(q))));

    var nextv: vec3<f32>;
    if spray {
        let factor = f32(e_pool.id) / AW_ID_TOP;
        let mind = max(spray_drag - spray_drag * spray_drag_variance, 0.0);
        let maxd = spray_drag + spray_drag * spray_drag_variance;
        let drag = mind + (1.0 - factor) * (maxd - mind);
        nextv = v + gravity * dt + (-drag * v * dt);
    } else {
        let vmac = aw_velocity(q, face_step);
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
    if travel >= AW_EPS {
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
            let phi = aw_solid(ww_cell_position(current, h));
            if phi < 0.0 {
                hit_phi = phi;
                found = true;
                break;
            }
            last = current;
        }
        if found {
            let reach = AW_MAX_RESOLVE * h;
            let grad = aw_gradient(ww_cell_position(current, h));
            if length(grad) > AW_EPS {
                let n = normalize(grad);
                resolved = current - (hit_phi - AW_SOLID_BUFFER * h) * n;
                if aw_solid(ww_cell_position(resolved, h)) < 0.0 || length(resolved - current) > reach {
                    resolved = last;
                }
                let u = dot(v, n) * n;
                bounce = (1.0 - spray_friction) * (v - u) - spray_restitution * u;
                bounced = true;
            } else {
                resolved = last;
            }
        }
    }
    if spray && bounced {
        nextv = bounce + gravity * dt;
    }
    if length(resolved - p) * (1.0 / dt) > AW_MAX_VELOCITY * length(nextv) {
        out.position_lifetime.w = AW_DEAD;
    }
    out.position_lifetime = vec4<f32>(resolved, out.position_lifetime.w);
    out.velocity = nextv;
    return out;
}

fn body(
    idx: u32,
    count: u32,
    e_pool: Element,
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
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
    cell_size: f32,
    pool_tiles: f32,
) -> Element {
    ww_pool_tiles = u32(pool_tiles);
    let steps = u32(max(substep_count, 0.0));
    let field_origin = vec3<f32>(bounds_lo_x, bounds_lo_y, bounds_lo_z);
    let dims = vec3<u32>(vec3<f32>(field_nodes_x,field_nodes_y,field_nodes_z));
    let stride = dims.x * dims.y * dims.z * 4u;
    let base = force_lattice_base(i32(tick_index), i32(first_tick), i32(force_lattices), dims);
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
            let c = force_lattice_corner(particle.position_lifetime.xyz, field_origin, field_spacing, dims, corner);
            for (var a = 0u; a < 3u; a = a + 1u) {
                if force_lattices > 0.0 {
                    acceleration[a] = fma(buf_forces[base + c.index * FORCE_NODE_WORDS + a], c.weight, acceleration[a]);
                }
                if (event & 0x80000000u) != 0u {
                    let at = (event & 0x7fffffffu) * stride + c.index * 4u + a;
                    if at < arrayLength(&buf_impulses) { impulse[a] = fma(buf_impulses[at], c.weight, impulse[a]); }
                }
            }
        }
        // Foam gets the impulse through the already-forced liquid velocity.
        if particle.kind != 1u && (event & 0x80000000u) != 0u { particle.velocity = particle.velocity + impulse; }
        particle = aw_step(particle, cell_size, acceleration.x, acceleration.y, acceleration.z, duration, foam_advection, bubble_buoyancy, bubble_drag, spray_drag, spray_drag_variance, spray_restitution, spray_friction, face_step);
    }
    return particle;
}
