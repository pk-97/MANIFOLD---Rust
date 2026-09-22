use crate::generators::mesh_common::PLATONIC_SHAPES;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::physics::RigidBody;
use crate::node_graph::primitive::Primitive;
use std::borrow::Cow;
crate::primitive! {
 name: RigidBodyNode,
 type_id: "node.rigid_body",
 purpose: "Describe a rigid body's shape, starting transform, motion type, mass and contact properties. Wire body into a shared Physics World; wire shape into a Platonic Solid Mesh to keep its collision hull and visible geometry aligned.",
 inputs: { transform: Transform required, mass: ScalarF32 optional, friction: ScalarF32 optional, bounce: ScalarF32 optional, },
 outputs: { body: RigidBody, shape: ScalarF32, },
 params: [
ParamDef { name: Cow::Borrowed("shape"), label: "Shape", ty: ParamType::Enum, default: ParamValue::Enum(1), range: Some((0.0, 4.0)), enum_values: PLATONIC_SHAPES },
ParamDef { name: Cow::Borrowed("motion"), label: "Motion", ty: ParamType::Enum, default: ParamValue::Enum(1), range: Some((0.0, 2.0)), enum_values: &["Fixed", "Dynamic", "Animated"] },
ParamDef { name: Cow::Borrowed("mass"), label: "Mass (kg)", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.01, 100.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("friction"), label: "Friction", ty: ParamType::Float, default: ParamValue::Float(0.5), range: Some((0.0, 1.0)), enum_values: &[] },
ParamDef { name: Cow::Borrowed("bounce"), label: "Bounce", ty: ParamType::Float, default: ParamValue::Float(0.15), range: Some((0.0, 1.0)), enum_values: &[] },
 ],
 depth_rule: Terminal,
 composition_notes: "One description per body. All bodies that should collide feed the same Physics World. Starting transform changes reposition that body; scale or shape changes rebuild the world. Dynamic bodies respond to gravity, Fixed bodies are static, Animated bodies follow the authored transform. Mesh radius must be 1; transform scale applies equally to visible mesh and collision hull.",
 examples: ["PhysicsSolids"],
 picker: { label: "Rigid Body", category: Atom },
 summary: "Give an object mass, friction and bounce, then connect it to a Physics World.",
 category: Geometry3D, role: Source,
 aliases: ["physics body", "rigid body", "collider"],
 boundary_reason: NonGpu,
}
impl Primitive for RigidBodyNode {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(transform) = ctx.inputs.transform("transform") else {
            ctx.error("Rigid Body needs a starting transform");
            return;
        };
        let selector = |name: &str| match ctx.params.get(name) {
            Some(ParamValue::Enum(v)) => *v,
            Some(ParamValue::Float(v)) => v.round() as u32,
            _ => 1,
        };
        let body = RigidBody {
            transform,
            shape: selector("shape"),
            kind: selector("motion"),
            mass: ctx.scalar_or_param("mass", 1.0),
            friction: ctx.scalar_or_param("friction", 0.5),
            bounce: ctx.scalar_or_param("bounce", 0.15),
        };
        ctx.outputs.set_rigid_body("body", body);
        ctx.outputs
            .set_scalar("shape", ParamValue::Float(body.shape as f32));
    }
}
