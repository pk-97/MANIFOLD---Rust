//! Small, explicit texture primitives for freeze compiler mechanics tests.
//!
//! These are hand-rolled instead of using `primitive!`: test-only nodes must
//! not enter the production primitive or catalog inventories.  Their `run`
//! methods are intentionally inert; the freeze tests exercise graph shape,
//! classification, code generation, and install-time retargeting.

#![cfg(test)]

use std::borrow::Cow;
use std::sync::OnceLock;

use crate::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, ParamValues};
use crate::freeze::classify::{BoundaryReason, FusionKind, InputAccess};
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::persistence::PrimitiveRegistry;
use crate::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::particles::Particle;
use crate::primitive::{Primitive, PrimitiveSpec};

const INPUTS: &[NodeInput] = &[NodePort {
    name: Cow::Borrowed("in"),
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
}];
const OUTPUTS: &[NodeOutput] = &[NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];
const PARAMS: &[ParamDef] = &[ParamDef {
    name: Cow::Borrowed("gain"),
    label: "Gain",
    ty: ParamType::Float,
    default: ParamValue::Float(1.0),
    range: Some((0.0, 4.0)),
    enum_values: &[],
}];
const NO_PARAMS: &[ParamDef] = &[];

/// Pointwise body used by all ordinary scalar fixture atoms.  Keeping the
/// parameter name stable makes install-time retarget assertions independent
/// of a production catalog node's parameter naming.
const POINTWISE_BODY: &str = "fn body(c_in: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, gain: f32) -> vec4<f32> { return c_in * gain; }";
const SOURCE_BODY: &str = "fn body(uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> { return vec4<f32>(uv, 0.0, 1.0); }";

pub struct TestFusionMap;

impl PrimitiveSpec for TestFusionMap {
    const TYPE_ID: &'static str = "test.fusion_map";
    const PURPOSE: &'static str = "TEST FIXTURE ONLY — scalar pointwise texture map for freeze mechanics.";
    const INPUTS: &'static [NodeInput] = INPUTS;
    const OUTPUTS: &'static [NodeOutput] = OUTPUTS;
    const PARAMS: &'static [ParamDef] = PARAMS;
    const FUSION_KIND: FusionKind = FusionKind::Pointwise;
    const DEPTH_RULE: crate::scene::depth_rule::DepthRule = crate::scene::depth_rule::DepthRule::Inherit;
    const WGSL_BODY: Option<&'static str> = Some(POINTWISE_BODY);

    fn cached_type_id() -> &'static EffectNodeType {
        static CELL: OnceLock<EffectNodeType> = OnceLock::new();
        CELL.get_or_init(|| EffectNodeType::new(Self::TYPE_ID))
    }
}

impl Primitive for TestFusionMap {
    fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

pub struct TestFusionBoundary;

impl PrimitiveSpec for TestFusionBoundary {
    const TYPE_ID: &'static str = "test.fusion_boundary";
    const PURPOSE: &'static str = "TEST FIXTURE ONLY — scalar texture boundary for freeze mechanics.";
    const INPUTS: &'static [NodeInput] = INPUTS;
    const OUTPUTS: &'static [NodeOutput] = OUTPUTS;
    const PARAMS: &'static [ParamDef] = NO_PARAMS;
    const FUSION_KIND: FusionKind = FusionKind::Boundary;
    const BOUNDARY_REASON: Option<BoundaryReason> = Some(BoundaryReason::Blocked);
    const DEPTH_RULE: crate::scene::depth_rule::DepthRule = crate::scene::depth_rule::DepthRule::Inherit;

    fn cached_type_id() -> &'static EffectNodeType {
        static CELL: OnceLock<EffectNodeType> = OnceLock::new();
        CELL.get_or_init(|| EffectNodeType::new(Self::TYPE_ID))
    }
}

impl Primitive for TestFusionBoundary {
    fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

pub struct TestFusionSource;

impl PrimitiveSpec for TestFusionSource {
    const TYPE_ID: &'static str = "test.fusion_source";
    const PURPOSE: &'static str = "TEST FIXTURE ONLY — source generator for freeze mechanics.";
    const INPUTS: &'static [NodeInput] = &[];
    const OUTPUTS: &'static [NodeOutput] = OUTPUTS;
    const PARAMS: &'static [ParamDef] = NO_PARAMS;
    const FUSION_KIND: FusionKind = FusionKind::Source;
    const DEPTH_RULE: crate::scene::depth_rule::DepthRule = crate::scene::depth_rule::DepthRule::Inherit;
    const WGSL_BODY: Option<&'static str> = Some(SOURCE_BODY);

    fn cached_type_id() -> &'static EffectNodeType {
        static CELL: OnceLock<EffectNodeType> = OnceLock::new();
        CELL.get_or_init(|| EffectNodeType::new(Self::TYPE_ID))
    }
}

impl Primitive for TestFusionSource {
    fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

pub struct TestFusionGather;

impl PrimitiveSpec for TestFusionGather {
    const TYPE_ID: &'static str = "test.fusion_gather";
    const PURPOSE: &'static str = "TEST FIXTURE ONLY — scalar texture gather seam for freeze mechanics.";
    const INPUTS: &'static [NodeInput] = INPUTS;
    const OUTPUTS: &'static [NodeOutput] = OUTPUTS;
    const PARAMS: &'static [ParamDef] = PARAMS;
    const FUSION_KIND: FusionKind = FusionKind::Pointwise;
    const INPUT_ACCESS: &'static [InputAccess] = &[InputAccess::Gather];
    const STENCIL_FETCH: bool = false;
    const DEPTH_RULE: crate::scene::depth_rule::DepthRule = crate::scene::depth_rule::DepthRule::Inherit;
    const WGSL_BODY: Option<&'static str> = Some("fn body(tex_in: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, gain: f32) -> vec4<f32> { return textureSampleLevel(tex_in, samp, uv, 0.0) * gain; }");

    fn cached_type_id() -> &'static EffectNodeType {
        static CELL: OnceLock<EffectNodeType> = OnceLock::new();
        CELL.get_or_init(|| EffectNodeType::new(Self::TYPE_ID))
    }
}

impl Primitive for TestFusionGather {
    fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

/// Add the fixture constructors to a test-local registry.  Keeping this
/// explicit avoids the global inventory/catalog side effects of production
/// primitive registration.
pub fn register_fusion_test_nodes(registry: &mut PrimitiveRegistry) {
    registry.register("test.fusion_map", || Box::new(TestFusionMap));
    registry.register("test.fusion_boundary", || Box::new(TestFusionBoundary));
    registry.register("test.fusion_source", || Box::new(TestFusionSource));
    registry.register("test.fusion_gather", || Box::new(TestFusionGather));
    registry.register("test.fusion_join", || Box::new(TestFusionJoin));
}

pub struct TestFusionJoin;

impl PrimitiveSpec for TestFusionJoin {
    const TYPE_ID: &'static str = "test.fusion_join";
    const PURPOSE: &'static str = "TEST FIXTURE ONLY — two-input coincident texture join for freeze mechanics.";
    const INPUTS: &'static [NodeInput] = &[
        NodePort { name: Cow::Borrowed("a"), ty: PortType::Texture2D, kind: PortKind::Input, required: true },
        NodePort { name: Cow::Borrowed("b"), ty: PortType::Texture2D, kind: PortKind::Input, required: true },
    ];
    const OUTPUTS: &'static [NodeOutput] = OUTPUTS;
    const PARAMS: &'static [ParamDef] = NO_PARAMS;
    const FUSION_KIND: FusionKind = FusionKind::MultiInputCoincident;
    const DEPTH_RULE: crate::scene::depth_rule::DepthRule = crate::scene::depth_rule::DepthRule::Inherit;
    const WGSL_BODY: Option<&'static str> = Some("fn body(c_a: vec4<f32>, c_b: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> { return (c_a + c_b) * 0.5; }");

    fn cached_type_id() -> &'static EffectNodeType {
        static CELL: OnceLock<EffectNodeType> = OnceLock::new();
        CELL.get_or_init(|| EffectNodeType::new(Self::TYPE_ID))
    }
}

impl Primitive for TestFusionJoin {
    fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

/// Buffer-domain identity fixture used by the substep border proof.  It has
/// the same one-array-in/one-array-out shape as a particle map, but no catalog
/// semantics or GPU implementation.
pub struct TestParticleMap;

impl EffectNode for TestParticleMap {
    fn type_id(&self) -> &EffectNodeType {
        static ID: OnceLock<EffectNodeType> = OnceLock::new();
        ID.get_or_init(|| EffectNodeType::new("test.particle_map"))
    }

    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: OnceLock<Vec<NodeInput>> = OnceLock::new();
        INPUTS.get_or_init(|| {
            let particles = PortType::Array(ArrayType::of_known::<Particle>());
            vec![
                NodePort { name: Cow::Borrowed("in"), ty: particles, kind: PortKind::Input, required: true },
                NodePort { name: Cow::Borrowed("forces"), ty: particles, kind: PortKind::Input, required: true },
            ]
        })
    }

    fn outputs(&self) -> &[NodeOutput] {
        static OUTPUTS: OnceLock<Vec<NodeOutput>> = OnceLock::new();
        OUTPUTS.get_or_init(|| vec![NodePort {
            name: Cow::Borrowed("out"),
            ty: PortType::Array(ArrayType::of_known::<Particle>()),
            kind: PortKind::Output,
            required: false,
        }])
    }

    fn parameters(&self) -> &[ParamDef] { &[] }

    fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}

    fn fusion_kind(&self) -> FusionKind { FusionKind::Pointwise }

    fn wgsl_body(&self) -> Option<&'static str> {
        Some("fn body(idx: u32, count: u32, e_in: Element, e_forces: Element) -> Element { return e_in; }")
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "out").then(|| {
            input_capacities
                .iter()
                .filter(|(name, _)| *name == "in" || *name == "forces")
                .map(|(_, n)| *n)
                .min()
        }).flatten()
    }

    fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule {
        crate::scene::depth_rule::DepthRule::Terminal
    }
}

pub fn register_particle_fusion_fixture(registry: &mut PrimitiveRegistry) {
    registry.register("test.particle_map", || Box::new(TestParticleMap));
}
