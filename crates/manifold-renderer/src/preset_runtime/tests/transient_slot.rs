//! Fixed-size source textures must retain their dimensions during slot reuse.
use super::*;
use crate::node_graph::primitives::{AudioSpectrum, Gain, Mix};
use crate::node_graph::{FinalOutput, Graph, Source, compile};

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
