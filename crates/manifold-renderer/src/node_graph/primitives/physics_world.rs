use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::physics::{BODY_PORTS, MAX_BODIES, POSE_PORTS, RigidSimulation};
use crate::node_graph::primitive::Primitive;
use std::borrow::Cow;
crate::primitive! {
 name: PhysicsWorldNode,
 type_id: "node.physics_world",
 purpose: "Advance one shared Box3D rigid-body world at fixed 120 Hz ticks and output its body transforms. Sixteen independently wired body descriptions share contacts. Gravity and simulation speed are live controls; Reset restores the authored starting poses.",
 inputs: {
body_0: RigidBody optional,
body_1: RigidBody optional,
body_2: RigidBody optional,
body_3: RigidBody optional,
body_4: RigidBody optional,
body_5: RigidBody optional,
body_6: RigidBody optional,
body_7: RigidBody optional,
body_8: RigidBody optional,
body_9: RigidBody optional,
body_10: RigidBody optional,
body_11: RigidBody optional,
body_12: RigidBody optional,
body_13: RigidBody optional,
body_14: RigidBody optional,
body_15: RigidBody optional,
gravity_x: ScalarF32 optional, gravity_y: ScalarF32 optional, gravity_z: ScalarF32 optional, speed: ScalarF32 optional, reset: ScalarF32 optional,
 },
 outputs: {
pose_0: Transform,
pose_1: Transform,
pose_2: Transform,
pose_3: Transform,
pose_4: Transform,
pose_5: Transform,
pose_6: Transform,
pose_7: Transform,
pose_8: Transform,
pose_9: Transform,
pose_10: Transform,
pose_11: Transform,
pose_12: Transform,
pose_13: Transform,
pose_14: Transform,
pose_15: Transform,
 },
 params: [
ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("gravity_y"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("speed"), label: "Simulation Speed", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("reset"), label: "Reset", ty: ParamType::Trigger, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
 ],
 depth_rule: Terminal,
 composition_notes: "Connect body_N to its matching pose_N consumer. Output transforms already include authored scale: connect directly to Scene Object transform, without applying that transform twice. State follows the transport clock; pause holds, reset/backward time restores initial poses. More than 128 pending ticks reports an error instead of silently dropping time. Shape/scale/topology edits rebuild this world; contact-property edits preserve motion. Native world stays private; no mutable handle wires.",
 examples: ["PhysicsSolids"],
 picker: { label: "Physics World", category: Atom },
 summary: "Simulate colliding objects together under gravity, with speed and reset controls.",
 category: Geometry3D, role: Filter,
 aliases: ["physics", "box3d", "rigid simulation"],
 boundary_reason: NonGpu,
 extra_fields: { simulation: RigidSimulation = RigidSimulation::default(), },
}
impl Primitive for PhysicsWorldNode {
    fn clear_state(&mut self) {
        self.simulation = RigidSimulation::default();
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let mut bodies = [None; MAX_BODIES];
        for (i, port) in BODY_PORTS.iter().enumerate() {
            bodies[i] = ctx.inputs.rigid_body(port);
        }
        let gravity = [
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity_y", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
        ];
        let speed = ctx.scalar_or_param("speed", 1.0);
        let reset = ctx.scalar_or_param("reset", 0.0);
        if let Err(error) = self
            .simulation
            .advance(bodies, gravity, ctx.time.seconds, speed, reset)
        {
            ctx.error(error);
        }
        for (port, pose) in POSE_PORTS.iter().zip(self.simulation.poses) {
            ctx.outputs.set_transform(port, pose);
        }
    }
}
