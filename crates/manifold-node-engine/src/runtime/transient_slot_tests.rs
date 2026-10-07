//! Fixed-size source textures must retain their dimensions during slot reuse.
use super::*;
use crate::primitives::{gain::Gain, mix::Mix};
use crate::testkit::graph::GraphFixture;
use crate::{scene::boundary_nodes::FinalOutput, graph::Graph, scene::boundary_nodes::Source, exec::execution_plan::compile};

#[test]
#[cfg(feature = "gpu-proofs")]
fn chain_reserves_provided_image_without_a_writable_backing() {
    let device = manifold_gpu::testkit::test_device();
    let primitives = PrimitiveRegistry::with_builtin();
    let mut fx = manifold_core::preset_definition_registry::create_default(&PresetTypeId::INVERT_COLORS);
    fx.graph = Some(serde_json::from_str(r#"{
        "version": 1, "name": "shared image chain",
        "nodes": [
            {"id": 0, "typeId": "system.source"},
            {"id": 1, "typeId": "node.gltf_texture_source", "handle": "image",
             "params": {"width": {"type": "Float", "value": 4.0},
                        "height": {"type": "Float", "value": 4.0}}},
            {"id": 2, "typeId": "node.mix"},
            {"id": 3, "typeId": "system.final_output"}
        ],
        "wires": [
            {"fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "a"},
            {"fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "b"},
            {"fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in"}
        ]
    }"#).unwrap());
    fx.graph_version += 1;
    fx.graph_structure_version += 1;
    let runtime = PresetRuntime::try_build(ChainBuildInputs {
        effects: std::slice::from_ref(&fx), groups: &[], primitives: &primitives,
        device: &device, pool: None, width: 8, height: 8, preview_effect: Some(&fx.id),
    }, None).expect("image chain builds");
    let image = runtime.effect_nodes[0].handles.iter()
        .find(|(handle, _)| handle.as_ref() == "image").unwrap().1;
    let resource = runtime.plan.steps().iter().find(|step| step.node == image)
        .unwrap().outputs[0].1;
    let backend = runtime.executor.backend();
    let slot = backend.slot_for(resource).unwrap();
    let descriptor = backend.provided_texture_descriptor(slot).expect("producer owns storage");
    assert_eq!((descriptor.width, descriptor.height, descriptor.mip_levels), (4, 4, 3));
    assert!(backend.texture_2d(slot).is_none(), "build must not allocate a duplicate image");
    let metal = backend.as_any().unwrap().downcast_ref::<MetalBackend>().unwrap();
    assert!(metal.render_target_2d(slot).is_none());
    let super::core::PresetIo::Transform { source_slot, output_slot, .. } = runtime.io else {
        panic!("chain must keep host-owned endpoints");
    };
    assert!(metal.render_target_2d(source_slot.expect("source-consuming chain")).is_some());
    assert!(metal.render_target_2d(output_slot).is_some());
}

#[test]
fn transient_slots_reuse_only_matching_resolved_dimensions() {
    let mut graph = Graph::new();
    let src = graph.add_node(Box::new(Source::new()));
    let spectrum = graph.add_node(Box::new(GraphFixture::texture_source(Some((512, 256)))));
    let mix = graph.add_node(Box::new(Mix::new()));
    let gain_1 = graph.add_node(Box::new(Gain::new()));
    let gain_2 = graph.add_node(Box::new(Gain::new()));
    let gain_3 = graph.add_node(Box::new(Gain::new()));
    let out = graph.add_node(Box::new(FinalOutput::new()));

    graph.connect((src, "out"), (mix, "a")).unwrap();
    graph.connect((spectrum, "out"), (mix, "b")).unwrap();
    graph.connect((mix, "out"), (gain_1, "in")).unwrap();
    graph.connect((gain_1, "out"), (gain_2, "in")).unwrap();
    graph.connect((gain_2, "out"), (gain_3, "in")).unwrap();
    graph.connect((gain_3, "out"), (out, "in")).unwrap();

    let plan = compile(&graph).expect("mixed fixed/canvas chain compiles");
    let source_res = plan
        .steps()
        .iter()
        .find(|step| step.node == src)
        .and_then(|step| step.outputs.iter().find(|(port, _)| *port == "out"))
        .map(|(_, res)| *res)
        .expect("source produces an out resource");
    let resource_for = |node| {
        plan.steps()
            .iter()
            .find(|step| step.node == node)
            .and_then(|step| step.outputs.iter().find(|(port, _)| *port == "out"))
            .map(|(_, res)| *res)
            .expect("node produces an out resource")
    };

    let assignment = assign_texture2d_slots(&plan, Some(source_res), (1080, 1920));
    let spectrum_res = resource_for(spectrum);
    let mix_res = resource_for(mix);
    let gain_1_res = resource_for(gain_1);
    let gain_2_res = resource_for(gain_2);
    let gain_3_res = resource_for(gain_3);

    let slot = |res| assignment.resource_to_slot[&res];
    assert_eq!(assignment.slot_dims[slot(spectrum_res).0 as usize], (512, 256));
    assert_eq!(assignment.slot_dims[slot(mix_res).0 as usize], (1080, 1920));
    assert_eq!(assignment.slot_dims[slot(gain_1_res).0 as usize], (1080, 1920));
    assert_eq!(assignment.slot_dims[slot(gain_2_res).0 as usize], (1080, 1920));
    assert_eq!(assignment.slot_dims[slot(gain_3_res).0 as usize], (1080, 1920));
    assert_ne!(slot(spectrum_res), slot(gain_1_res));

    // The canvas chain still recycles its transient slots once their
    // lifetimes end; the fixed-size spectrum slot cannot be substituted.
    assert_eq!(slot(gain_2_res), slot(mix_res));
    assert_eq!(slot(gain_3_res), slot(gain_1_res));
    assert_eq!(assignment.slot_count, 4);
}

#[test]
fn source_independent_plan_does_not_allocate_external_source_slot() {
    let mut graph = Graph::new();
    let source = graph.add_node(Box::new(Source::new()));
    let checker = graph.add_node(Box::new(GraphFixture::texture_source(None)));
    let output = graph.add_node(Box::new(FinalOutput::new()));
    graph.connect((checker, "out"), (output, "in")).unwrap();

    let plan = compile(&graph).expect("source-independent chain compiles");
    assert!(plan
        .steps()
        .iter()
        .find(|step| step.node == source)
        .is_none_or(|step| step.outputs.is_empty()));

    let assignment = assign_texture2d_slots(&plan, None, (64, 64));
    assert_eq!(assignment.source_slot, None);
    assert!(!assignment.resource_to_slot.is_empty());
    assert_eq!(assignment.slot_count as usize, assignment.slot_dims.len());
}

/// Never ready, like an async source whose load has not landed.
#[cfg(feature = "gpu-proofs")]
struct AlwaysPending(crate::exec::effect_node::EffectNodeType);

#[cfg(feature = "gpu-proofs")]
impl crate::exec::effect_node::EffectNode for AlwaysPending {
    fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule {
        crate::scene::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &crate::exec::effect_node::EffectNodeType { &self.0 }
    fn inputs(&self) -> &[crate::ports::NodeInput] { &[] }
    fn outputs(&self) -> &[crate::ports::NodeOutput] {
        use crate::ports::{NodePort, PortKind, PortType};
        static OUTPUTS: [crate::ports::NodeOutput; 1] = [NodePort {
            name: std::borrow::Cow::Borrowed("out"), ty: PortType::Texture2D,
            kind: PortKind::Output, required: false,
        }];
        &OUTPUTS
    }
    fn parameters(&self) -> &[crate::parameters::ParamDef] { &[] }
    fn evaluate(&mut self, ctx: &mut crate::exec::effect_node::EffectNodeContext<'_, '_>) {
        ctx.mark_outputs_pending();
    }
}

/// A chain whose last step is held publishes no output, so the layer
/// composites its input unprocessed instead of whatever another resource
/// left in the recycled output slot.
#[test]
#[cfg(feature = "gpu-proofs")]
fn held_terminal_publishes_no_chain_output() {
    let device = manifold_gpu::testkit::test_device();
    let mut primitives = PrimitiveRegistry::with_builtin();
    primitives.register("test.always_pending", || {
        Box::new(AlwaysPending(crate::exec::effect_node::EffectNodeType::new("test.always_pending")))
    });
    let mut fx = manifold_core::preset_definition_registry::create_default(&PresetTypeId::INVERT_COLORS);
    fx.graph = Some(serde_json::from_str(r#"{
        "version": 1, "name": "held terminal chain",
        "nodes": [
            {"id": 0, "typeId": "system.source"},
            {"id": 1, "typeId": "test.always_pending"},
            {"id": 2, "typeId": "node.mix"},
            {"id": 3, "typeId": "system.final_output"}
        ],
        "wires": [
            {"fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "a"},
            {"fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "b"},
            {"fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in"}
        ]
    }"#).unwrap());
    fx.graph_version += 1;
    fx.graph_structure_version += 1;
    let mut runtime = PresetRuntime::try_build(ChainBuildInputs {
        effects: std::slice::from_ref(&fx), groups: &[], primitives: &primitives,
        device: &device, pool: None, width: 8, height: 8, preview_effect: Some(&fx.id),
    }, None).expect("held chain builds");
    let input = RenderTarget::new(&device, 8, 8, GpuTextureFormat::Rgba16Float, "held-chain-input");
    let ctx = PresetContext {
        time: 0.0, beat: 0.0, dt: 1.0 / 60.0, width: 8, height: 8,
        output_width: 8, output_height: 8, aspect: 1.0, owner_key: 0,
        is_clip_level: false, frame_count: 0, anim_progress: 0.0, trigger_count: 0,
    };
    let mut enc = device.create_encoder("held-chain");
    let published = {
        let mut gpu = GpuEncoder::new(&mut enc, &device);
        runtime.run(&mut gpu, &input.texture, std::slice::from_ref(&fx), &[], &ctx).is_some()
    };
    enc.commit_and_wait_completed();
    assert!(!published, "a held terminal publishes nothing");
    assert!(runtime.output_texture().is_none());
}
