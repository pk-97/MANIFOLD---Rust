//! Port and parameter fixtures for engine mechanics; never inventory-registered.
use std::borrow::Cow;
use crate::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, ParamValues};
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType};

pub(crate) struct GraphFixture {
    id: EffectNodeType,
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
    params: Vec<ParamDef>,
    feedback: bool,
    provided: bool,
    dims: Option<(u32, u32)>,
    capacity: Option<u32>,
}

fn port(name: &'static str, ty: PortType, kind: PortKind) -> NodePort {
    NodePort { name: Cow::Borrowed(name), ty, kind, required: kind == PortKind::Input }
}

impl GraphFixture {
    fn new(id: &'static str, inputs: Vec<NodeInput>, outputs: Vec<NodeOutput>) -> Self {
        Self { id: EffectNodeType::new(id), inputs, outputs, params: vec![], feedback: false, provided: false, dims: None, capacity: None }
    }

    pub(crate) fn binding() -> Self {
        let mut node = Self::texture_filter();
        node.params = [("scale", 1.0), ("translate_x", 0.0), ("translate_y", 0.0), ("rotation", 0.0)]
            .into_iter().map(|(name, value)| ParamDef {
                name: Cow::Borrowed(name), label: name, ty: ParamType::Float,
                default: ParamValue::Float(value), range: Some(match name { "scale" => (0.1, 5.0), "rotation" => (-180.0, 180.0), _ => (-1.0, 1.0) }), enum_values: &[],
            }).collect();
        node
    }

    pub(crate) fn texture_filter() -> Self {
        Self::new("test.texture_filter", vec![port("in", PortType::Texture2D, PortKind::Input)], vec![port("out", PortType::Texture2D, PortKind::Output)])
    }

    pub(crate) fn feedback() -> Self {
        let mut node = Self::texture_filter();
        node.feedback = true;
        node
    }

    pub(crate) fn texture_source(dims: Option<(u32, u32)>) -> Self {
        let mut node = Self::new("test.texture_source", vec![], vec![port("out", PortType::Texture2D, PortKind::Output)]);
        node.dims = dims;
        node
    }

    pub(crate) fn array_feedback() -> Self {
        let ty = PortType::Array(ArrayType::of_known::<crate::particles::Particle>());
        let mut node = Self::new("test.array_feedback", vec![port("in", ty, PortKind::Input)], vec![port("out", ty, PortKind::Output)]);
        node.feedback = true;
        node
    }

    #[cfg(feature = "gpu-proofs")]
    pub(crate) fn provided_texture() -> Self {
        let mut node = Self::texture_source(Some((1024, 1024)));
        node.provided = true;
        node
    }

    pub(crate) fn particles() -> Self {
        let mut node = Self::new("test.spawn_particles", vec![], vec![port("particles", PortType::Array(ArrayType::of_known::<crate::particles::Particle>()), PortKind::Output)]);
        node.capacity = Some(4);
        node
    }

    pub(crate) fn forces() -> Self {
        let mut node = Self::new("test.grid_uv_field", vec![], vec![port("uv", PortType::Array(ArrayType::of_known::<[f32; 2]>()), PortKind::Output)]);
        node.capacity = Some(4);
        node
    }

    pub(crate) fn particle_step() -> Self {
        Self::new("test.move_particles", vec![
            port("in", PortType::Array(ArrayType::of_known::<crate::particles::Particle>()), PortKind::Input),
            port("forces", PortType::Array(ArrayType::of_known::<[f32; 2]>()), PortKind::Input),
        ], vec![port("out", PortType::Array(ArrayType::of_known::<crate::particles::Particle>()), PortKind::Output)])
    }
}

impl EffectNode for GraphFixture {
    fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule { crate::scene::depth_rule::DepthRule::Terminal }
    fn type_id(&self) -> &EffectNodeType { &self.id }
    fn inputs(&self) -> &[NodeInput] { &self.inputs }
    fn outputs(&self) -> &[NodeOutput] { &self.outputs }
    fn parameters(&self) -> &[ParamDef] { &self.params }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) { panic!("graph-only fixture must not execute"); }
    fn breaks_dependency_cycle(&self) -> bool { self.feedback }
    fn state_capture_input_ports(&self) -> &[&str] { if self.feedback { &["in"] } else { &[] } }
    fn persistent_output_ports(&self) -> &[&str] { if self.feedback { &["out"] } else { &[] } }
    fn provides_texture_output(&self, port: &str) -> bool { self.provided && port == "out" }
    fn output_mipmapped(&self, port: &str) -> bool { self.provided && port == "out" }
    fn output_dims(&self, _: &str, _: (u32, u32), _: &[(&str, (u32, u32))], _: &ParamValues) -> Option<(u32, u32)> { self.dims }
    fn array_output_capacity(&self, _: &str, _: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        self.capacity.or_else(|| inputs.iter().find(|(name, _)| *name == "in").map(|(_, n)| *n))
    }
}
