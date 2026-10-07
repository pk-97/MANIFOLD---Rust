use manifold_node_engine::runtime::{PresetRuntime, ChainBuildInputs};
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_core::PresetTypeId;
use manifold_node_engine::exec::metal_backend::MetalBackend;
use manifold_node_engine::gpu::{gpu_encoder::GpuEncoder, render_target::RenderTarget};
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_gpu::GpuTextureFormat;
#[cfg(feature = "gpu-proofs")]
struct AlwaysPending(manifold_node_engine::exec::effect_node::EffectNodeType);

#[cfg(feature = "gpu-proofs")]
impl manifold_node_engine::exec::effect_node::EffectNode for AlwaysPending {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &manifold_node_engine::exec::effect_node::EffectNodeType { &self.0 }
    fn inputs(&self) -> &[manifold_node_engine::ports::NodeInput] { &[] }
    fn outputs(&self) -> &[manifold_node_engine::ports::NodeOutput] {
        use manifold_node_engine::ports::{NodePort, PortKind, PortType};
        static OUTPUTS: [manifold_node_engine::ports::NodeOutput; 1] = [NodePort {
            name: std::borrow::Cow::Borrowed("out"), ty: PortType::Texture2D,
            kind: PortKind::Output, required: false,
        }];
        &OUTPUTS
    }
    fn parameters(&self) -> &[manifold_node_engine::parameters::ParamDef] { &[] }
    fn evaluate(&mut self, ctx: &mut manifold_node_engine::exec::effect_node::EffectNodeContext<'_, '_>) {
        ctx.mark_outputs_pending();
    }
}

/// A chain whose last step is held publishes no output, so the layer
/// composites its input unprocessed instead of whatever another resource
/// left in the recycled output slot.


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
    let image = runtime.effect_slots_for_test()[0].handles_for_test().iter()
        .find(|(handle, _)| handle.as_ref() == "image").unwrap().1;
    let resource = runtime.plan.steps().iter().find(|step| step.node == image)
        .unwrap().outputs[0].1;
    let backend = runtime.backend_for_test();
    let slot = backend.slot_for(resource).unwrap();
    let descriptor = backend.provided_texture_descriptor(slot).expect("producer owns storage");
    assert_eq!((descriptor.width, descriptor.height, descriptor.mip_levels), (4, 4, 3));
    assert!(backend.texture_2d(slot).is_none(), "build must not allocate a duplicate image");
    let metal = backend.as_any().unwrap().downcast_ref::<MetalBackend>().unwrap();
    assert!(metal.render_target_2d(slot).is_none());
    let Some((source_slot, output_slot)) = runtime.transform_slots_for_test() else {
        panic!("chain must keep host-owned endpoints");
    };
    assert!(metal.render_target_2d(source_slot.expect("source-consuming chain")).is_some());
    assert!(metal.render_target_2d(output_slot).is_some());
}
#[test]
#[cfg(feature = "gpu-proofs")]
fn held_terminal_publishes_no_chain_output() {
    let device = manifold_gpu::testkit::test_device();
    let mut primitives = PrimitiveRegistry::with_builtin();
    primitives.register("test.always_pending", || {
        Box::new(AlwaysPending(manifold_node_engine::exec::effect_node::EffectNodeType::new("test.always_pending")))
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