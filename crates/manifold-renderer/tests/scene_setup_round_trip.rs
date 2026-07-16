//! SCENE_SETUP_PANEL_DESIGN.md P1 round-trip gate (DESIGN_DOC_STANDARD §5):
//! a Scene Setup panel edit must survive save → reload, not just the create
//! path. Builds a generator layer with a per-instance graph override holding
//! an atmosphere node, "edits" its fog density the same way
//! `SetGraphNodeParamCommand` would (writes the node's `params` map), saves
//! the project as plain V1 JSON, reloads it, and asserts
//! `SceneVm::from_def` on the RELOADED layer's generator graph still shows
//! the edited value — proving both persistence and that the panel's
//! `SceneVm` re-derivation isn't silently stale after a reload.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_renderer::node_graph::scene_vm::{AtmosphereVm, SceneVm};

fn node(id: u32, type_id: &str, params: BTreeMap<String, SerializedParamValue>) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: manifold_core::NodeId::new(format!("n{id}")),
        type_id: type_id.to_string(),
        handle: Some(format!("n{id}")),
        params,
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    }
}

/// A minimal graph: a bare `render_scene` (no objects/lights) with a wired
/// `node.atmosphere`, at the given `fog_density`.
fn def_with_fog(fog_density: f32) -> EffectGraphDef {
    let mut fog_params = BTreeMap::new();
    fog_params.insert("fog_density".to_string(), SerializedParamValue::Float { value: fog_density });
    let fog = node(1, "node.atmosphere", fog_params);

    let mut scene_params = BTreeMap::new();
    scene_params.insert("objects".to_string(), SerializedParamValue::Float { value: 0.0 });
    let scene = node(2, "node.render_scene", scene_params);

    let output = node(3, "system.final_output", BTreeMap::new());

    EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: None,
        nodes: vec![fog, scene, output],
        wires: vec![
            wire(1, "atmosphere", 2, "atmosphere"),
            wire(2, "color", 3, "in"),
        ],
    }
}

#[test]
fn scene_setup_fog_edit_survives_save_reload_and_scene_vm_re_shows_it() {
    let mut project = Project::default();
    // `add_layer`'s own generator_type arg is the "New 3D Scene" assignment
    // step; the panel's Fog density drag then writes the node param — exactly
    // what `SetGraphNodeParamCommand` does to `layer.gen_params_or_init().graph`.
    let idx = project.timeline.add_layer(
        "Scene",
        LayerType::Generator,
        PresetTypeId::from_string("SceneStarter".to_string()),
    );
    {
        let layer = &mut project.timeline.layers[idx];
        layer.gen_params_or_init().graph = Some(def_with_fog(0.37));
    }

    // Pre-save sanity: the Vm already shows the edited value.
    {
        let layer = &project.timeline.layers[idx];
        let def = layer.generator_graph().expect("graph override present");
        let vm = SceneVm::from_def(def).expect("scene found");
        match vm.atmosphere {
            AtmosphereVm::Wired(row) => assert!((row.density_value - 0.37).abs() < 1e-6),
            AtmosphereVm::None => panic!("expected wired atmosphere before save"),
        }
    }

    let path = std::env::temp_dir().join(format!(
        "manifold_scene_setup_round_trip_{}_{}.manifold",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    manifold_io::saver::save_project_v1(&project, &path).expect("save v1");

    let reloaded = manifold_io::loader::load_project(&path);
    let _ = std::fs::remove_file(&path);
    let reloaded = reloaded.expect("reload");
    let layer = reloaded
        .timeline
        .layers
        .iter()
        .find(|l| l.layer_type == LayerType::Generator)
        .expect("generator layer survived reload");
    let def = layer
        .generator_graph()
        .expect("graph override survived reload — the panel's edit is not silently dropped");
    let vm = SceneVm::from_def(def).expect("scene still resolves after reload");
    match vm.atmosphere {
        AtmosphereVm::Wired(row) => assert!(
            (row.density_value - 0.37).abs() < 1e-6,
            "fog density must round-trip exactly; got {}",
            row.density_value
        ),
        AtmosphereVm::None => panic!(
            "atmosphere wire must survive the round trip — the panel would silently \
             show 'None' + an Add Fog button instead of the user's edit"
        ),
    }
}
