// node.matter_move_bodies — fusable BUFFER body, GATHER (GPU_MPM_SOLVER_DESIGN.md
// section 4.1 step 2). One thread per body: its pose at the end of this
// substep s, t = (s + 1)·dt after the tick starts, from the domain's row for
// this tick. Matches liquid::bodies::body_pose_at for prescribed bodies.
//
// Prescribed (w of position_inv_mass = 0): translation along the linear
// velocity, rotation by the constant angular velocity, which is the slerp
// between the tick's end poses.
//
// Dynamic (1/m > 0, a coupled body at its centre of mass): symplectic Euler
// per substep from the tick-start state, with the external accelerations
// (accel_shape.xyz, the inv_inertia rows' w) and the liquid's reaction from
// the substeps before this one. `reaction` holds, per body, D = Σ Δv_j and
// W = Σ (j/n)·Δv_j over substeps j < s (node.matter_body_reaction runs after
// this atom), and the same pair for the angular impulse times 1/m over dx.
// With n·W = Σ j·Δv_j:
//   v_s = v0 + (s+1)·a·dt + D
//   x_s = x0 + dt·((s+1)·v0 + a·dt·(s+1)(s+2)/2 + s·D − n·W)
// and likewise ω and the turn, Δω = I⁻¹·L with I⁻¹ the tick-start world
// inverse inertia. The row carries v_s and ω_s out, the body's velocity for
// the collider projection.
//
// ABI: `bodies` (LiquidBody → Element) is gathered at row
// (tick_index − first_tick)·body_count + idx; a row past `rows` comes out
// disabled (accel_shape.w = −1). `reaction` (16 words per body) is read only
// when dynamic_count > 0.

fn move_reaction(base: u32, scale: f32) -> vec3<f32> {
    return vec3<f32>(
        f32(buf_reaction[base]),
        f32(buf_reaction[base + 1u]),
        f32(buf_reaction[base + 2u]),
    ) * scale;
}

fn body(
    idx: u32,
    count: u32,
    tick_index: i32,
    first_tick: i32,
    substep_in_tick: i32,
    step_dt: f32,
    body_count: i32,
    rows: i32,
    substeps_per_tick: i32,
    momentum_unit: f32,
    cell_size: f32,
    dynamic_count: i32,
) -> Element {
    let row = u32(max(tick_index - first_tick, 0)) * u32(max(body_count, 0)) + idx;
    if row >= u32(max(rows, 0)) {
        var off: Element;
        off.accel_shape = vec4<f32>(0.0, 0.0, 0.0, -1.0);
        return off;
    }
    var b = buf_bodies[row];
    let steps = f32(substep_in_tick + 1);
    let inv_mass = b.position_inv_mass.w;
    if !(inv_mass > 0.0) {
        let t = steps * step_dt;
        b.position_inv_mass = vec4<f32>(
            b.position_inv_mass.xyz + b.linear_velocity.xyz * t,
            b.position_inv_mass.w,
        );
        b.rotation = liquid_turn(b.rotation, b.angular_velocity.xyz, t);
        return b;
    }

    var d = vec3<f32>(0.0);
    var w = vec3<f32>(0.0);
    var dl = vec3<f32>(0.0);
    var wl = vec3<f32>(0.0);
    if dynamic_count > 0 {
        let base = idx * 16u;
        let scale = momentum_unit / 16777216.0;
        d = move_reaction(base, scale);
        w = move_reaction(base + 3u, scale);
        dl = move_reaction(base + 6u, scale);
        wl = move_reaction(base + 9u, scale);
    }
    let s = f32(substep_in_tick);
    let n = f32(max(substeps_per_tick, 1));
    let ramp = steps * (steps + 1.0) * 0.5;

    let a = b.accel_shape.xyz;
    let v0 = b.linear_velocity.xyz;
    let velocity = v0 + steps * step_dt * a + d;
    let moved = step_dt * (steps * v0 + step_dt * ramp * a + s * d - n * w);

    // L·(1/m)/dx back to L, then Δω = I⁻¹·L (rows).
    let to_impulse = cell_size / inv_mass;
    let inertia_x = b.inv_inertia_x.xyz;
    let inertia_y = b.inv_inertia_y.xyz;
    let inertia_z = b.inv_inertia_z.xyz;
    let ld = dl * to_impulse;
    let lw = wl * to_impulse;
    let dw = vec3<f32>(dot(inertia_x, ld), dot(inertia_y, ld), dot(inertia_z, ld));
    let ww = vec3<f32>(dot(inertia_x, lw), dot(inertia_y, lw), dot(inertia_z, lw));
    let alpha = vec3<f32>(b.inv_inertia_x.w, b.inv_inertia_y.w, b.inv_inertia_z.w);
    let w0 = b.angular_velocity.xyz;
    let omega = w0 + steps * step_dt * alpha + dw;
    let turn = step_dt * (steps * w0 + step_dt * ramp * alpha + s * dw - n * ww);

    b.position_inv_mass = vec4<f32>(b.position_inv_mass.xyz + moved, inv_mass);
    b.rotation = liquid_turn(b.rotation, turn, 1.0);
    b.linear_velocity = vec4<f32>(velocity, b.linear_velocity.w);
    b.angular_velocity = vec4<f32>(omega, b.angular_velocity.w);
    return b;
}
