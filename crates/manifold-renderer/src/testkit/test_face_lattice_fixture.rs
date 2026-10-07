//! TEST-ONLY FIXTURE for BUG-u8io (fft-water-fusion-param-capacity): a
//! lattice-sized fusable member, its count Π (nodes + 1) from its params
//! (`ParamProduct` with plus 1, a face grid), that reads one face array at
//! `[idx]` and gathers one f32 array. The region compiler's lattice tests
//! chain two of them.
//!
//! Hand-rolled `PrimitiveSpec`, not `crate::primitive!`, for the reason
//! `test_camera_pointwise_fixture` gives: the macro auto-registers into the
//! global inventories the catalog freshness tests walk. Compile-level only:
//! nothing dispatches it.

#![cfg(test)]

use std::borrow::Cow;
use std::sync::OnceLock;

use crate::node_graph::{EffectNodeContext, EffectNodeType, ParamValues};
use crate::node_graph::fluid_particles::FaceSample;
use crate::node_graph::freeze::classify::{FusedOutputCapacity, FusionKind, InputAccess};
use crate::node_graph::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::primitive::{Primitive, PrimitiveSpec};

pub const TYPE_ID: &str = "test.face_lattice";

#[derive(Default)]
pub struct TestFaceLattice;

const INPUTS: &[NodeInput] = &[
    NodePort {
        name: Cow::Borrowed("faces"),
        ty: PortType::Array(ArrayType::of_known::<FaceSample>()),
        kind: PortKind::Input,
        required: true,
    },
    NodePort {
        name: Cow::Borrowed("water"),
        ty: PortType::Array(ArrayType::of_known::<f32>()),
        kind: PortKind::Input,
        required: true,
    },
];
const OUTPUTS: &[NodeOutput] = &[NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Array(ArrayType::of_known::<FaceSample>()),
    kind: PortKind::Output,
    required: false,
}];
const fn side(name: &'static str) -> ParamDef {
    ParamDef {
        name: Cow::Borrowed(name),
        label: name,
        ty: ParamType::Float,
        default: ParamValue::Float(8.0),
        range: Some((1.0, 1024.0)),
        enum_values: &[],
    }
}
const LATTICE_PARAMS: [&str; 3] = ["nodes_x", "nodes_y", "nodes_z"];
const PARAMS: &[ParamDef] = &[side("nodes_x"), side("nodes_y"), side("nodes_z")];

const WGSL_BODY: &str = "\
fn body(idx: u32, count: u32, e_faces: Element, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> Element {
    var f = e_faces;
    f.velocity.x = f.velocity.x + buf_water[idx % arrayLength(&buf_water)];
    return f;
}";

impl PrimitiveSpec for TestFaceLattice {
    const TYPE_ID: &'static str = TYPE_ID;
    const PURPOSE: &'static str = "TEST FIXTURE ONLY (BUG-u8io) — a face grid sized by its lattice params that adds a gathered water value to each face's x velocity.";
    const INPUTS: &'static [NodeInput] = INPUTS;
    const OUTPUTS: &'static [NodeOutput] = OUTPUTS;
    const PARAMS: &'static [ParamDef] = PARAMS;
    const FUSION_KIND: FusionKind = FusionKind::Pointwise;
    const DEPTH_RULE: crate::node_graph::depth_rule::DepthRule =
        crate::node_graph::depth_rule::DepthRule::Terminal;
    const WGSL_BODY: Option<&'static str> = Some(WGSL_BODY);
    const INPUT_ACCESS: &'static [InputAccess] = &[InputAccess::Coincident, InputAccess::BufferGather];
    const FUSED_OUTPUT_CAPACITY: FusedOutputCapacity =
        FusedOutputCapacity::ParamProduct { params: &LATTICE_PARAMS, plus: 1 };

    fn cached_type_id() -> &'static EffectNodeType {
        static CELL: OnceLock<EffectNodeType> = OnceLock::new();
        CELL.get_or_init(|| EffectNodeType::new(Self::TYPE_ID))
    }
}

impl Primitive for TestFaceLattice {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        let side = |name: &str| match params.get(name) {
            Some(ParamValue::Float(n)) => Some(n.round() as u32 + 1),
            _ => None,
        };
        let faces = LATTICE_PARAMS.iter().try_fold(1u32, |product, name| product.checked_mul(side(name)?));
        (port == "out").then_some(faces).flatten()
    }

    fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}
