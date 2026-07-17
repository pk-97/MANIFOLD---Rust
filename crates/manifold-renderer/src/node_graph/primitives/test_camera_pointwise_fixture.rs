//! TEST-ONLY FIXTURE (D7/P0 I6 proof, `docs/CINEMATIC_POST_DESIGN.md`).
//!
//! P0's fusion layer needs to prove its per-member camera-derived-uniform
//! recompute mechanism works for a camera-derived POINTWISE **TEXTURE** atom —
//! the shape P1's real deliverable (`coc_from_depth`) will eventually take.
//! `coc_from_depth` doesn't exist yet (P1 is a later, separately-briefed
//! phase; building it here would be scope creep into that phase's
//! deliverable). This is the minimal stand-in: one texture in, one Camera in,
//! adds the camera's `pos.x` to every texel via a `derived_uniforms` field —
//! same shape as `coc_from_depth`'s `camera: Camera required` +
//! `derived_uniforms` contract, none of its actual lens math.
//!
//! `#![cfg(test)]` on the WHOLE FILE — this primitive is never registered in
//! the real palette/catalog/inventory outside test builds.
//!
//! **Hand-rolled `PrimitiveSpec`/`Primitive` impl, deliberately NOT the
//! `crate::primitive!` macro.** The macro auto-`inventory::submit!`s to TWO
//! global channels — `PrimitiveFactory` (so `PrimitiveRegistry::with_builtin()`
//! discovers it) AND `NodeDescriptor` (so `catalog_gen` includes it in
//! `docs/node_catalog.json`) — with no opt-out (`docs/ADDING_PRIMITIVES.md`:
//! "auto-register... nothing else has to be edited"). Even with `picker`
//! omitted, `catalog_gen`'s completeness tests
//! (`palette_visible_nodes_have_complete_descriptors`,
//! `regenerates_in_sync`) walk `NodeDescriptor` for EVERY registered node,
//! not just palette-visible ones, so a macro-declared fixture registered in
//! any test build goes red against the checked-in catalog JSON the moment
//! `cargo test --features gpu-proofs` compiles this file. Implementing the
//! traits directly (the blanket `impl<P: Primitive> EffectNode for P` in
//! `node_graph::primitive` still applies automatically — no `EffectNode` impl
//! needed either) keeps this fixture out of BOTH inventory channels; the I6
//! test in `freeze/proof.rs` makes it constructible by building a registry
//! with `PrimitiveRegistry::with_builtin().register("test.camera_pointwise",
//! ...)` rather than relying on global auto-discovery.

#![cfg(test)]

use std::borrow::Cow;
use std::sync::OnceLock;

use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuSampler, GpuSamplerDesc};

use crate::node_graph::effect_node::{EffectNodeContext, EffectNodeType};
use crate::node_graph::freeze::classify::FusionKind;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::primitive::{Primitive, PrimitiveSpec};

pub struct TestCameraPointwise {
    pub pipeline: Option<GpuComputePipeline>,
    pub sampler: Option<GpuSampler>,
}

impl TestCameraPointwise {
    pub fn new() -> Self {
        Self { pipeline: None, sampler: None }
    }
}

impl Default for TestCameraPointwise {
    fn default() -> Self {
        Self::new()
    }
}

const INPUTS: &[NodeInput] = &[
    NodePort {
        name: Cow::Borrowed("in"),
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: true,
    },
    NodePort {
        name: Cow::Borrowed("camera"),
        ty: PortType::Camera,
        kind: PortKind::Input,
        required: true,
    },
];
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
const WGSL_BODY: &str = "fn body(c_in: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, gain: f32, cam_x: f32) -> vec4<f32> {\n    return vec4<f32>(c_in.rgb * gain + vec3<f32>(cam_x, 0.0, 0.0), c_in.a);\n}";
const DERIVED_UNIFORMS: &[&str] = &["cam_x"];

impl PrimitiveSpec for TestCameraPointwise {
    const TYPE_ID: &'static str = "test.camera_pointwise";
    const PURPOSE: &'static str = "TEST FIXTURE ONLY (D7/P0 I6, docs/CINEMATIC_POST_DESIGN.md) — adds the wired Camera's pos.x to every texel's red channel, scaled by gain. Proves the freeze compiler's fusion-layer camera-derived-uniform recompute mechanism for a camera-derived Pointwise TEXTURE atom, standing in for P1's not-yet-built coc_from_depth.";
    const INPUTS: &'static [NodeInput] = INPUTS;
    const OUTPUTS: &'static [NodeOutput] = OUTPUTS;
    const PARAMS: &'static [ParamDef] = PARAMS;
    const FUSION_KIND: FusionKind = FusionKind::Pointwise;
    const DEPTH_RULE: crate::node_graph::depth_rule::DepthRule =
        crate::node_graph::depth_rule::DepthRule::Terminal;
    const WGSL_BODY: Option<&'static str> = Some(WGSL_BODY);
    const DERIVED_UNIFORMS: &'static [&'static str] = DERIVED_UNIFORMS;

    fn cached_type_id() -> &'static EffectNodeType {
        static CELL: OnceLock<EffectNodeType> = OnceLock::new();
        CELL.get_or_init(|| EffectNodeType::new(Self::TYPE_ID))
    }
}

// D7/P0: the fixture's own recompute registration — reads the region's routed
// Camera external's `pos.x`, matching `run()`'s own computation below exactly.
// (This is a DIFFERENT inventory channel — `derived_uniform_registry`'s own —
// which `catalog_gen` never walks, so it's safe to register globally even
// though the primitive itself is deliberately kept out of `PrimitiveFactory`
// / `NodeDescriptor` above.)
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "test.camera_pointwise",
        recompute: |ctx| ctx.camera.map(|c| vec![c.pos[0]]),
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TestCameraPointwiseUniforms {
    gain: f32,
    cam_x: f32,
    _pad0: f32,
    _pad1: f32,
}

impl Primitive for TestCameraPointwise {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let gain = ctx.scalar_or_param("gain", 1.0);
        let cam = ctx
            .inputs
            .camera("camera")
            .unwrap_or_else(crate::node_graph::camera::Camera::default_perspective);
        let Some(in_tex) = ctx.inputs.texture_2d("in") else { return };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else { return };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory): the runtime kernel is generated from
            // `wgsl_body`, so this fixture proves the SAME standalone-codegen
            // path (with derived_uniforms) any real camera-derived texture
            // atom (coc_from_depth, P1) will use.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("test.camera_pointwise standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "test.camera_pointwise",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = TestCameraPointwiseUniforms {
            gain,
            cam_x: cam.pos[0],
            _pad0: 0.0,
            _pad1: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Texture { binding: 1, texture: in_tex },
                GpuBinding::Sampler { binding: 2, sampler },
                GpuBinding::Texture { binding: 3, texture: out_tex },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "test.camera_pointwise",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;

    #[test]
    fn fixture_declares_camera_and_texture_ports() {
        assert_eq!(TestCameraPointwise::TYPE_ID, "test.camera_pointwise");
        assert_eq!(TestCameraPointwise::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(TestCameraPointwise::INPUTS[1].ty, PortType::Camera);
        assert_eq!(TestCameraPointwise::DERIVED_UNIFORMS, &["cam_x"]);
        let prim = TestCameraPointwise::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "test.camera_pointwise");
    }

    #[test]
    fn uniform_struct_is_16_bytes() {
        assert_eq!(std::mem::size_of::<TestCameraPointwiseUniforms>(), 16);
    }

    #[test]
    fn fixture_is_not_globally_registered() {
        // The whole point of hand-rolling instead of using `crate::primitive!`:
        // this type_id must NOT resolve through the global builtin registry
        // (that would mean it leaked into PrimitiveFactory/NodeDescriptor —
        // catalog_gen would then see it and go red against the checked-in
        // docs/node_catalog.json).
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        assert!(
            !registry.contains("test.camera_pointwise"),
            "the I6 fixture must stay OUT of the global builtin registry"
        );
    }
}
