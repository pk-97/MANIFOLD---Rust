// `node.interpolate_particle_frames` — keyed cubic-Hermite interpolation at
// the particle-frame seam (GPU_FLUID_SURFACE_DESIGN.md D11).
//
// `particles_b` is the coincident stream and therefore supplies e_particles_b.
// `particles_a` is BufferGather: frames are sorted by nonzero id, so the body
// can find a match with a bounded binary search. An unwired A is represented
// by count_a = 0: every B record is then rewound like a birth but keeps its
// radius, since with no frame A nothing was just born (whitewater publishes
// no A, and its display blend sits at 0 once an interval behind transport).

fn b_count(value: f32, capacity: u32) -> u32 {
    if value < 0.0 {
        return capacity;
    }
    if !(value >= 0.0) {
        return 0u;
    }
    return min(u32(min(value, f32(capacity))), capacity);
}

fn a_count(value: f32, capacity: u32) -> u32 {
    if !(value >= 0.0) {
        return 0u;
    }
    return min(u32(min(value, f32(capacity))), capacity);
}

fn safe_blend(value: f32) -> f32 {
    if !(value >= 0.0) {
        return 0.0;
    }
    return min(value, 1.0);
}

fn safe_span(value: f32) -> f32 {
    if !(value > 0.0) {
        return 0.0;
    }
    return value;
}

fn zero_particle() -> Element {
    return Element(vec4<f32>(0.0), vec3<f32>(0.0), 0u);
}

fn hermite_particle(a: Element, b: Element, t: f32, span: f32) -> Element {
    if span == 0.0 {
        return Element(b.position_radius, b.velocity, b.id);
    }

    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    let position = h00 * a.position_radius.xyz
        + h10 * span * a.velocity
        + h01 * b.position_radius.xyz
        + h11 * span * b.velocity;

    // Derivative of the Hermite curve with respect to physical time.  The
    // position terms divide by span; this helper is only called for span > 0.
    let dh00 = (6.0 * t2 - 6.0 * t) / span;
    let dh10 = 3.0 * t2 - 4.0 * t + 1.0;
    let dh01 = (-6.0 * t2 + 6.0 * t) / span;
    let dh11 = 3.0 * t2 - 2.0 * t;
    let velocity = dh00 * a.position_radius.xyz
        + dh10 * a.velocity
        + dh01 * b.position_radius.xyz
        + dh11 * b.velocity;
    let radius = mix(a.position_radius.w, b.position_radius.w, t);
    return Element(vec4<f32>(position, radius), velocity, b.id);
}

fn birth_particle(b: Element, t: f32, span: f32, acceleration: vec3<f32>, grow: f32) -> Element {
    let tau = (1.0 - t) * span;
    let position = b.position_radius.xyz - tau * b.velocity + 0.5 * acceleration * tau * tau;
    let velocity = b.velocity - acceleration * tau;
    return Element(vec4<f32>(position, b.position_radius.w * grow), velocity, b.id);
}

fn body(
    idx: u32,
    count: u32,
    e_particles_b: Element,
    count_a: f32,
    count_b: f32,
    identity_a: f32,
    identity_b: f32,
    blend: f32,
    span: f32,
    acceleration_x: f32,
    acceleration_y: f32,
    acceleration_z: f32,
) -> Element {
    // `count` is the output-capacity anchor from particles_b.  Keeping this
    // bound independent of the coincident input global is required when the
    // atom fuses: a coincident producer is a register, not a storage binding.
    let b_limit = b_count(count_b, count);
    if idx >= b_limit || !(e_particles_b.position_radius.w > 0.0) {
        return zero_particle();
    }

    let t = safe_blend(blend);
    let h = safe_span(span);
    let a_limit = a_count(count_a, arrayLength(&buf_particles_a));
    var matched = false;
    var a_particle = zero_particle();
    if identity_a == identity_b && e_particles_b.id != 0u {
        var low = 0u;
        var high = a_limit;
        loop {
            if low >= high {
                break;
            }
            let middle = low + (high - low) / 2u;
            let middle_id = buf_particles_a[middle].id;
            if middle_id < e_particles_b.id {
                low = middle + 1u;
            } else {
                high = middle;
            }
        }
        if low < a_limit {
            let candidate = buf_particles_a[low];
            if candidate.id == e_particles_b.id && candidate.id != 0u {
                matched = true;
                a_particle = candidate;
            }
        }
    }

    if matched {
        return hermite_particle(a_particle, e_particles_b, t, h);
    }
    return birth_particle(
        e_particles_b,
        t,
        h,
        vec3<f32>(acceleration_x, acceleration_y, acceleration_z),
        select(1.0, t, a_limit > 0u),
    );
}
