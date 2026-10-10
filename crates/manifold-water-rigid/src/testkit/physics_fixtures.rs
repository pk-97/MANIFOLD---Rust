//! Minimal CPU controls for history/carry mechanics, independent of family nodes.
use std::borrow::Cow;
use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use manifold_node_engine::scene::{depth_rule::DepthRule, transform::Transform};
use crate::physics::RigidBody;

#[derive(Clone, Copy)]
enum Kind { Transform, Body, Field, Wave }
struct Fixture { kind: Kind, id: EffectNodeType }
const fn input(name: &'static str, ty: PortType) -> NodeInput {
    NodePort { name: Cow::Borrowed(name), ty, kind: PortKind::Input, required: true }
}
const fn output(name: &'static str, ty: PortType) -> NodeOutput {
    NodePort { name: Cow::Borrowed(name), ty, kind: PortKind::Output, required: false }
}
const fn param(name: &'static str) -> ParamDef {
    ParamDef { name: Cow::Borrowed(name), label: name, ty: ParamType::Float,
        default: ParamValue::Float(0.0), range: None, enum_values: &[] }
}
static BODY_INPUTS: &[NodeInput] = &[input("transform", PortType::Transform)];
static WAVE_INPUTS: &[NodeInput] = &[input("clock", PortType::Scalar(ScalarType::F32))];
static TRANSFORM_OUTPUTS: &[NodeOutput] = &[output("transform", PortType::Transform)];
static BODY_OUTPUTS: &[NodeOutput] = &[output("body", PortType::RigidBody)];
static FIELD_OUTPUTS: &[NodeOutput] = &[output("out", PortType::VectorField)];
static WAVE_OUTPUTS: &[NodeOutput] = &[output("out", PortType::Scalar(ScalarType::F32))];
static TRANSFORM_PARAMS: &[ParamDef] = &[param("pos_x"), param("pos_y")];
static FIELD_PARAMS: &[ParamDef] = &[param("x"), param("y")];
impl EffectNode for Fixture {
    fn type_id(&self) -> &EffectNodeType { &self.id }
    fn depth_rule(&self) -> DepthRule { DepthRule::Terminal }
    fn is_pure(&self) -> bool { true }
    fn inputs(&self) -> &[NodeInput] {
        match self.kind {
            Kind::Body => BODY_INPUTS,
            Kind::Wave => WAVE_INPUTS,
            _ => &[],
        }
    }
    fn outputs(&self) -> &[NodeOutput] {
        match self.kind {
            Kind::Transform => TRANSFORM_OUTPUTS,
            Kind::Body => BODY_OUTPUTS,
            Kind::Field => FIELD_OUTPUTS,
            Kind::Wave => WAVE_OUTPUTS,
        }
    }
    fn parameters(&self) -> &[ParamDef] {
        match self.kind {
            Kind::Transform => TRANSFORM_PARAMS,
            Kind::Field => FIELD_PARAMS,
            _ => &[],
        }
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        match self.kind {
            Kind::Transform => {
                let transform = Transform { pos: [ctx.param_f32("pos_x", 0.0), ctx.param_f32("pos_y", 0.0), 0.0], ..Transform::default() };
                ctx.outputs.set_transform("transform", transform);
            }
            Kind::Body => {
                let body = RigidBody { transform: ctx.inputs.transform("transform").expect("fixture transform"), ..RigidBody::default() };
                ctx.outputs.set_cpu_value("body", body);
            }
            Kind::Field => {
                let field = manifold_physics::FieldValue::uniform([ctx.param_f32("x", 0.0), ctx.param_f32("y", 0.0), 0.0]).expect("finite fixture field");
                ctx.outputs.set_cpu_value("out", field);
            }
            Kind::Wave => {
                let Some(ParamValue::Float(clock)) = ctx.inputs.scalar("clock") else { panic!("fixture clock") };
                ctx.outputs.set_scalar("out", ParamValue::Float((clock * 12.0).sin()));
            }
        }
    }
}
pub fn register(registry: &mut PrimitiveRegistry) {
    for (id, kind) in [("test.physics_transform", Kind::Transform), ("test.physics_body", Kind::Body),
        ("test.physics_field", Kind::Field), ("test.physics_wave", Kind::Wave)] {
        registry.register(id, move || Box::new(Fixture { kind, id: EffectNodeType::new(id) }));
    }
}
