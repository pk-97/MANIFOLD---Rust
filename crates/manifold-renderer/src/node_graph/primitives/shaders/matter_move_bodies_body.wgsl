// node.matter_move_bodies — fusable BUFFER body, GATHER (GPU_MPM_SOLVER_DESIGN.md
// section 4.1 step 2). One thread per body: its pose at the end of this
// substep, t = (substep_in_tick + 1)·step_dt after the tick starts, from the
// domain's row for this tick (tick-start pose and the motion over the tick):
// translation along the linear velocity, rotation by the constant angular
// velocity, which is the slerp between the tick's end poses. Every other
// field passes through. Matches matter::body_pose_at.
//
// ABI: `bodies` (MatterBody → Element) is gathered at row
// (tick_index − first_tick)·body_count + idx; a row past `rows` comes out
// disabled (accel_shape.w = −1).
fn body(
    idx: u32,
    count: u32,
    tick_index: i32,
    first_tick: i32,
    substep_in_tick: i32,
    step_dt: f32,
    body_count: i32,
    rows: i32,
) -> Element {
    let row = u32(max(tick_index - first_tick, 0)) * u32(max(body_count, 0)) + idx;
    if row >= u32(max(rows, 0)) {
        var off: Element;
        off.accel_shape = vec4<f32>(0.0, 0.0, 0.0, -1.0);
        return off;
    }
    var b = buf_bodies[row];
    let t = f32(substep_in_tick + 1) * step_dt;
    b.position_inv_mass = vec4<f32>(
        b.position_inv_mass.xyz + b.linear_velocity.xyz * t,
        b.position_inv_mass.w,
    );
    b.rotation = matter_turn(b.rotation, b.angular_velocity.xyz, t);
    return b;
}
