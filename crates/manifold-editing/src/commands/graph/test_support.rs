//! Shared test fixtures for the `commands/graph/` command modules.
//!
//! Distributed here in P2-G/S7 (pure move) from the flat `mod tests` that
//! lived in `graph.rs`. Every fixture is imported by two or more sibling test
//! mods via `use super::super::test_support::*`; single-consumer fixtures stay
//! beside their module's tests. Bodies are verbatim; the only edits are the
//! module dedent and a `pub(super)` on each fixture so sibling test mods reach
//! it (`pub(super)` = visible within `graph` and all its descendants).

use super::*;
use manifold_core::EffectId;
use manifold_core::LayerId;
use manifold_core::layer::Layer;
use manifold_core::types::LayerType;
use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION;
use manifold_core::effects::PresetInstance;

pub(super) fn slot(id: &str, value: f32, exposed: bool) -> manifold_core::params::Param {
    let mut p = manifold_core::params::Param::bundled(manifold_core::effect_graph_def::ParamSpecDef {
        id: id.into(),
        name: id.into(),
        min: 0.0,
        max: 1.0,
        default_value: value,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: vec![],
        format_string: None,
        osc_suffix: String::new(),
        curve: Default::default(),
        invert: false,
        is_angle: false,
        is_trigger_gate: false,
        wraps: false,
        section: None,
        card_visible: true,
    });
    p.value = value;
    p.base = value;
    p.exposed = exposed;
    p
}

pub(super) fn abc_graph() -> EffectGraphDef {
    let mk = |id: u32, handle: &str, ty: &str| EffectGraphNode {
        id,
        node_id: manifold_core::NodeId::new(handle),
        type_id: ty.to_string(),
        handle: Some(handle.to_string()),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    };
    let w = |fln: u32, fp: &str, tn: u32, tp: &str| EffectGraphWire {
        from_node: fln,
        from_port: fp.to_string(),
        to_node: tn,
        to_port: tp.to_string(),
    };
    EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        nodes: vec![
            mk(0, "a", "system.source"),
            mk(1, "b", "node.transform"),
            mk(2, "c", "system.final_output"),
        ],
        wires: vec![w(0, "out", 1, "in"), w(1, "out", 2, "in")],
    }
}

pub(super) fn project_with_graph(def: EffectGraphDef) -> (Project, EffectId) {
    let mut project = Project::default();
    let effect_id = EffectId::new("test-group-fx");
    let mut fx = PresetInstance::new(PresetTypeId::new("test.fx"));
    fx.id = effect_id.clone();
    fx.graph = Some(def);
    project.settings.master_effects.push(fx);
    (project, effect_id)
}

pub(super) fn graph_of<'a>(project: &'a Project, id: &EffectId) -> &'a EffectGraphDef {
    project
        .find_effect_by_id(id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap()
}

/// Catalog default for a Mirror-like graph: source → uv_transform
/// → mix → final_output, four nodes plus four wires. Mirrors the
/// shape the runtime `build_mirror` produces.
pub(super) fn mirror_catalog_default() -> EffectGraphDef {
    let mut def = EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        nodes: vec![
            EffectGraphNode {
                id: 0,
                node_id: manifold_core::NodeId::default(),
                type_id: "system.source".to_string(),
                handle: Some("source".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
            EffectGraphNode {
                id: 1,
                node_id: manifold_core::NodeId::default(),
                type_id: "node.transform".to_string(),
                handle: Some("uv_transform".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
            EffectGraphNode {
                id: 2,
                node_id: manifold_core::NodeId::default(),
                type_id: "node.mix".to_string(),
                handle: Some("mix".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
            EffectGraphNode {
                id: 3,
                node_id: manifold_core::NodeId::default(),
                type_id: "system.final_output".to_string(),
                handle: Some("final_output".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
        ],
        wires: vec![
            EffectGraphWire {
                from_node: 0,
                from_port: "out".to_string(),
                to_node: 1,
                to_port: "source".to_string(),
            },
            EffectGraphWire {
                from_node: 0,
                from_port: "out".to_string(),
                to_node: 2,
                to_port: "a".to_string(),
            },
            EffectGraphWire {
                from_node: 1,
                from_port: "out".to_string(),
                to_node: 2,
                to_port: "b".to_string(),
            },
            EffectGraphWire {
                from_node: 2,
                from_port: "out".to_string(),
                to_node: 3,
                to_port: "in".to_string(),
            },
        ],
    };
    // Stamp node ids == handle, matching the bundled-preset convention
    // (a node's stable id is its authoring handle).
    for n in &mut def.nodes {
        if let Some(h) = n.handle.clone() {
            n.node_id = manifold_core::NodeId::new(h);
        }
    }
    def
}

/// Project with one timeline layer, no generator override.
pub(super) fn project_with_one_generator_layer() -> (Project, LayerId) {
    let mut project = Project::default();
    let layer = Layer::new("Test Layer".to_string(), LayerType::Generator, 0);
    let lid = layer.layer_id.clone();
    project.timeline.layers.push(layer);
    (project, lid)
}

/// A single-param `SceneParamMetadata` fixture — stands in for what
/// `manifold_renderer::node_graph::scene_exposure::metadata_for_node_type`
/// would compute from a real primitive's `ParamDef` (this crate can't
/// depend on the renderer, so the app-side caller is the real source —
/// see the cross-crate constraint note in
/// `docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` P1).
pub(super) fn scene_param_meta(name: &str, label: &str) -> manifold_core::scene_exposure::SceneParamMetadata {
    manifold_core::scene_exposure::SceneParamMetadata {
        name: name.to_string(),
        label: label.to_string(),
        min: 0.0,
        max: 1.0,
        default_value: SerializedParamValue::Float { value: 0.5 },
        is_angle: false,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        convert: manifold_core::effects::ParamConvert::Float,
    }
}
