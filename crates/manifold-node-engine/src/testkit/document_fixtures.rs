//! Explicit texture/parameter fixture for document mechanics.
use std::borrow::Cow;
use crate::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::persistence::PrimitiveRegistry;

struct DocumentFixture(EffectNodeType);
const INPUTS: &[NodeInput] = &[NodePort { name: Cow::Borrowed("in"), ty: PortType::Texture2D, kind: PortKind::Input, required: false }];
const OUTPUTS: &[NodeOutput] = &[NodePort { name: Cow::Borrowed("out"), ty: PortType::Texture2D, kind: PortKind::Output, required: false }];
const PARAMS: &[ParamDef] = &[
    ParamDef { name: Cow::Borrowed("roughness"), label: "Appearance", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
    ParamDef { name: Cow::Borrowed("scale"), label: "Scale", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
    ParamDef { name: Cow::Borrowed("a"), label: "A", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
];
impl EffectNode for DocumentFixture {
    fn type_id(&self) -> &EffectNodeType { &self.0 }
    fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule { crate::scene::depth_rule::DepthRule::Inherit }
    fn inputs(&self) -> &[NodeInput] { INPUTS }
    fn outputs(&self) -> &[NodeOutput] { OUTPUTS }
    fn parameters(&self) -> &[ParamDef] { PARAMS }
    fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}
#[cfg(test)]
pub(crate) fn registry() -> PrimitiveRegistry {
    let mut registry = PrimitiveRegistry::with_builtin();
    register(&mut registry);
    registry
}
pub fn register(registry: &mut PrimitiveRegistry) {
    registry.register("test.document", || Box::new(DocumentFixture(EffectNodeType::new("test.document"))));
}
