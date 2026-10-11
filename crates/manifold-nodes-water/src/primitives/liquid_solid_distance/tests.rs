//! FLIP's solid atom against Matter's body mover: the proof names both
//! solvers, so it links from the registration crate (WATER_CRATES_DESIGN.md D8).
use manifold_node_engine::primitive::PrimitiveSpec;
use manifold_water_gpu_flip::primitives::liquid_solid_distance::LiquidSolidDistance;
use manifold_water_gpu_mpm::primitives::matter_move_bodies::MatterMoveBodies;

/// The solid atom poses bodies the way node.matter_move_bodies does:
/// both turn through the shared pose library.
#[test]
fn liquid_solid_distance_poses_bodies_as_move_bodies() {
    let solid = <LiquidSolidDistance as PrimitiveSpec>::WGSL_BODY.expect("liquid_solid_distance body");
    let moving = <MatterMoveBodies as PrimitiveSpec>::WGSL_BODY.expect("matter_move_bodies body");
    // Adaptive steps pose at the accumulated elapsed time; fixed steps
    // retain the supplied tick duration. Translation and rotation agree.
    assert!(solid.contains("select(tick_seconds, clock_plan[0].elapsed, clock_plan[0].live_mode != 0u)"));
    assert!(solid.contains("let pose_time = adaptive_tick_seconds(tick_seconds)"));
    assert!(solid.contains("bd.position_inv_mass.xyz + bd.linear_velocity.xyz * pose_time"));
    assert!(solid.contains("liquid_turn(bd.rotation, bd.angular_velocity.xyz, pose_time)"));
    assert!(moving.contains("liquid_turn(b.rotation, b.angular_velocity.xyz, t)"));
    assert!(!solid.contains("sin(0.5 * angle)") && !moving.contains("sin(0.5 * angle)"));
}
