//! Fixed-size source textures must retain their dimensions during slot reuse.
use super::*;
use crate::node_graph::primitives::{AudioSpectrum, Gain, Mix};
use crate::node_graph::{FinalOutput, Graph, Source, compile};

#[test]
fn chain_reserves_provided_image_without_a_writable_backing() {
    let device = crate::test_device();
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
    let super::core::PresetIo::Transform { source_slot, output_slot } = runtime.io else {
        panic!("chain must keep host-owned endpoints");
    };
    assert!(metal.render_target_2d(source_slot).is_some());
    assert!(metal.render_target_2d(output_slot).is_some());
}

#[test]
fn transient_slots_reuse_only_matching_resolved_dimensions() {
    let mut graph = Graph::new();
    let src = graph.add_node(Box::new(Source::new()));
    let spectrum = graph.add_node(Box::new(AudioSpectrum::new()));
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

    let assignment = assign_texture2d_slots(&plan, source_res, (1080, 1920));
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
