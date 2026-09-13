//! CPU conformance for an authored Surface Peel clip-hit-return variation.
//!
//! The promoted stock file proves that file loading, scene attachment,
//! canonical preparation, and the mock runtime carry a clip edge through the
//! authored trigger gate and beat envelope.

use std::path::Path;
use std::sync::Mutex;

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::params::{Param, ParamManifest};
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::EmbeddedOrigin;
use manifold_core::scene_modifier_edit::insert_scene_modifier;
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneStageScope, SceneTargetSelection};
use manifold_core::{Beats, NodeId, Seconds};
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::scene_modifier_authoring::prepare_new_scene_modifier;
use manifold_renderer::node_graph::scene_modifier_expand::{
    PreparedModifierEvents, PreparedSceneModifierGraph, SceneModifierNodeRoute,
    prepare_scene_modifiers,
};
use manifold_renderer::node_graph::{FrameTime, NodeInstanceId, PrimitiveRegistry};
use manifold_renderer::preset_loader::{
    SCENE_MODIFIER_CATALOG, clear_project_presets, set_project_presets,
};
use manifold_renderer::preset_runtime::{FrameContextInputs, PresetRuntime};

const MUSHROOM_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/cc0___mushroom.glb"
);
const SURFACE_PEEL_HIT_ID: &str = "SurfacePeelHit";
const SURFACE_PEEL_HIT: &str = include_str!("../assets/scene-modifier-presets/SurfacePeelHit.json");

static CATALOG_TEST_LOCK: Mutex<()> = Mutex::new(());

struct CatalogGuard;

impl Drop for CatalogGuard {
    fn drop(&mut self) {
        clear_project_presets();
    }
}

fn install_recipe_in_catalog() -> CatalogGuard {
    set_project_presets(
        Vec::new(),
        Vec::new(),
        vec![(
            SURFACE_PEEL_HIT_ID.to_string(),
            SURFACE_PEEL_HIT.to_string(),
            EmbeddedOrigin::Saved,
        )],
    );
    assert!(
        SCENE_MODIFIER_CATALOG
            .load()
            .json(SURFACE_PEEL_HIT_ID)
            .is_some(),
        "project scene-modifier catalog must expose authored fixture"
    );
    CatalogGuard
}

fn recipe() -> EffectGraphDef {
    let id = PresetTypeId::from_string(SURFACE_PEEL_HIT_ID.to_string());
    let json = SCENE_MODIFIER_CATALOG
        .load()
        .json(id.as_str())
        .expect("SurfacePeelHit resolves from the project catalog");
    serde_json::from_str(&json).expect("catalog recipe parses")
}

fn mushroom_host() -> EffectGraphDef {
    let (graph, _) = assemble_import_graph(Path::new(MUSHROOM_FIXTURE))
        .expect("production mushroom import must assemble");
    graph
}

fn render_scene(host: &EffectGraphDef) -> SceneNodeRef {
    let scene = host
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .expect("mushroom host has render_scene");
    SceneNodeRef {
        scope: Vec::new(),
        node: scene.node_id.clone(),
    }
}

fn attached_host() -> EffectGraphDef {
    let host = mushroom_host();
    let instance = prepare_new_scene_modifier(
        &host,
        &recipe(),
        NodeId::new("surface-peel-hit"),
        render_scene(&host),
        SceneTargetSelection::AllObjects,
    )
    .expect("authored hit recipe prepares against mushroom");
    assert!(!instance.mesh_frames.is_empty(), "mushroom capture is real");
    insert_scene_modifier(&host, 0, instance)
        .expect("authored hit recipe attaches through core editing")
        .graph
}

fn exact_node(runtime: &PresetRuntime, node_id: &NodeId) -> NodeInstanceId {
    runtime
        .graph
        .instance_by_node_id(node_id)
        .unwrap_or_else(|| panic!("runtime node {node_id} is present"))
}

fn live(runtime: &PresetRuntime, id: NodeInstanceId, param: &str) -> f32 {
    let node_id = runtime
        .graph
        .get_node(id)
        .expect("live node")
        .node_id
        .clone();
    runtime
        .live_node_params_watched()
        .into_iter()
        .find(|(id, _)| *id == node_id)
        .and_then(|(_, values)| {
            values
                .into_iter()
                .find(|(name, _)| *name == param)
                .map(|(_, value)| value)
        })
        .unwrap_or_else(|| panic!("live parameter {node_id}.{param} is present"))
}

fn binding_id(def: &EffectGraphDef, modifier_id: &str, param_id: &str) -> String {
    def.preset_metadata
        .as_ref()
        .expect("host metadata")
        .bindings
        .iter()
        .find_map(|binding| {
            matches!(
                &binding.target,
                BindingTarget::SceneModifier {
                    modifier_id: owner,
                    param_id: parameter,
                } if owner == &NodeId::new(modifier_id) && parameter == param_id
            )
            .then(|| binding.id.clone())
        })
        .unwrap_or_else(|| {
            panic!("exact scene-modifier binding {modifier_id}/{param_id} is present")
        })
}

fn manifest_with(def: &EffectGraphDef, modifier_id: &str, values: &[(&str, f32)]) -> ParamManifest {
    let metadata = def.preset_metadata.as_ref().expect("host metadata");
    let mut manifest = ParamManifest::from_params(
        metadata
            .params
            .iter()
            .cloned()
            .map(Param::bundled)
            .collect(),
    );
    for (param_id, value) in values {
        let id = binding_id(def, modifier_id, param_id);
        let param = manifest
            .get_mut(&id)
            .unwrap_or_else(|| panic!("host manifest parameter {id} is present"));
        param.value = *value;
        param.base = *value;
    }
    manifest
}

fn route_for<'a>(
    prepared: &'a PreparedSceneModifierGraph,
    modifier_id: &str,
    scope: &str,
    node_id: &str,
) -> &'a SceneModifierNodeRoute {
    prepared
        .routes
        .iter()
        .find(|route| {
            route.modifier_id == NodeId::new(modifier_id)
                && route.local.scope == [NodeId::new(scope)]
                && route.local.node == NodeId::new(node_id)
        })
        .unwrap_or_else(|| panic!("scoped route {modifier_id}/{scope}/{node_id} is present"))
}

fn generated_id(prepared: &PreparedSceneModifierGraph, node_id: &NodeId) -> u32 {
    prepared
        .def
        .nodes
        .iter()
        .find(|node| &node.node_id == node_id)
        .unwrap_or_else(|| panic!("prepared node {node_id} is present"))
        .id
}

fn reaches_source(
    prepared: &PreparedSceneModifierGraph,
    target: u32,
    target_port: &str,
    source: u32,
) -> bool {
    let mut frontier = vec![target];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(node) = frontier.pop() {
        if !seen.insert(node) {
            continue;
        }
        for wire in
            prepared.def.wires.iter().filter(|wire| {
                wire.to_node == node && (node != target || wire.to_port == target_port)
            })
        {
            if wire.from_node == source {
                return true;
            }
            frontier.push(wire.from_node);
        }
    }
    false
}

fn frame(
    runtime: &mut PresetRuntime,
    events: &mut PreparedModifierEvents,
    beat: f32,
    clip_edge: bool,
    clip_gate: bool,
) {
    runtime.set_frame_context(FrameContextInputs {
        time: beat,
        beat,
        aspect: 1.0,
        trigger_count: 0.0,
        anim_progress: 0.0,
        output_width: 1920.0,
        output_height: 1080.0,
    });
    if clip_edge {
        assert!(
            events.note_clip(|_| clip_gate) == clip_gate,
            "clip edge follows the authored gate state"
        );
    }
    events.write_context(&mut runtime.graph);
    runtime.execute_frame(FrameTime {
        beats: Beats(f64::from(beat)),
        seconds: Seconds(f64::from(beat)),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    });
    events.consume_pending();
}

#[test]
fn authored_hit_fixture_loads_attaches_and_routes_two_stage_event_path() {
    let _lock = CATALOG_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _catalog = install_recipe_in_catalog();
    let attached = attached_host();
    let registry = PrimitiveRegistry::with_builtin();
    let prepared = prepare_scene_modifiers(&attached, &registry)
        .expect("attached authored hit recipe prepares canonically");

    let recipe_metadata = attached.scene_modifiers[0]
        .graph
        .preset_metadata
        .as_ref()
        .expect("modifier metadata")
        .scene_modifier
        .as_ref()
        .expect("modifier recipe");
    assert_eq!(
        recipe_metadata.stages.len(),
        2,
        "control and per-object stages"
    );
    assert_eq!(recipe_metadata.stages[0].scope, SceneStageScope::Scene);
    assert_eq!(recipe_metadata.stages[1].scope, SceneStageScope::EachObject);
    assert!(matches!(
        recipe_metadata.stages[1].inputs.iter().find(|input| input.port == "burst"),
        Some(input) if matches!(
            &input.source,
            manifold_core::scene_modifier_preset::SceneStageSource::StageOutput { stage, port }
                if stage == &NodeId::new("hit_control_stage") && port == "burst"
        )
    ));

    assert_eq!(
        prepared.event_routes.len(),
        1,
        "one modifier owns one event route"
    );
    assert_eq!(
        prepared
            .def
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.trigger_gate")
            .count(),
        1,
        "one shared Scene trigger gate"
    );
    assert_eq!(
        prepared
            .def
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.envelope_beats")
            .count(),
        1,
        "one shared Scene envelope"
    );
    assert!(
        prepared
            .def
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.math")
            .count()
            >= 3
    );
    for id in [
        "lift",
        "curl",
        "clip_trigger",
        "burst_strength",
        "burst_duration",
    ] {
        let binding = binding_id(&attached, "surface-peel-hit", id);
        assert!(binding.starts_with("sceneModifier:"));
    }
    let patch_route = route_for(&prepared, "surface-peel-hit", "peel_stage", "patch");
    assert!(
        patch_route.copies.len() >= 2,
        "mushroom has material/object copies"
    );
    assert!(patch_route.copies.iter().all(|copy| copy.object.is_some()));
    let hit_route = route_for(
        &prepared,
        "surface-peel-hit",
        "hit_control_stage",
        "hit_scale",
    );
    assert_eq!(hit_route.copies.len(), 1, "Scene control is evaluated once");
    assert!(hit_route.copies[0].object.is_none());
    let lift_route = route_for(&prepared, "surface-peel-hit", "peel_stage", "lift_add");
    assert_eq!(lift_route.copies.len(), patch_route.copies.len());
    let shared_burst = generated_id(&prepared, &hit_route.copies[0].node_id);
    for copy in &lift_route.copies {
        assert!(
            reaches_source(
                &prepared,
                generated_id(&prepared, &copy.node_id),
                "b",
                shared_burst
            ),
            "each object copy must consume the shared Scene burst"
        );
    }
    let event_route = prepared
        .event_routes
        .iter()
        .find(|route| route.modifier_id == NodeId::new("surface-peel-hit"))
        .expect("prepared trigger/baseline event route");

    let mut manifest = manifest_with(
        &attached,
        "surface-peel-hit",
        &[("lift", 0.27), ("curl", 0.75), ("clip_trigger", 1.0)],
    );
    let mut runtime = PresetRuntime::from_def(attached.clone(), &registry, Some(&manifest))
        .expect("authored hit runtime loads through canonical from_def");
    let mut events =
        PreparedModifierEvents::prepare(&attached, &prepared.event_routes, &runtime.graph)
            .expect("runtime event route resolves");
    assert!(
        runtime
            .graph
            .instance_by_node_id(&event_route.count_node)
            .is_some()
    );
    assert!(
        runtime
            .graph
            .instance_by_node_id(&event_route.baseline_node)
            .is_some()
    );
    let patch_nodes: Vec<_> = patch_route
        .copies
        .iter()
        .map(|copy| exact_node(&runtime, &copy.node_id))
        .collect();
    assert_eq!(patch_nodes.len(), lift_route.copies.len());

    let assert_all = |runtime: &PresetRuntime, separation: f32, rotation: f32| {
        for patch in &patch_nodes {
            assert!((live(runtime, *patch, "separation") - separation).abs() < 1e-5);
            assert!((live(runtime, *patch, "rotation") - rotation).abs() < 1e-5);
        }
    };

    frame(&mut runtime, &mut events, 0.0, false, true);
    assert_all(&runtime, 0.27, 0.75);

    // A cold edge starts from the zero baseline and reaches every object copy.
    frame(&mut runtime, &mut events, 0.0, true, true);
    assert_all(&runtime, 0.62, 1.10);

    // The held base values may change while the burst decays.
    manifest = manifest_with(
        &attached,
        "surface-peel-hit",
        &[("lift", 0.40), ("curl", 0.90), ("clip_trigger", 1.0)],
    );
    runtime.apply_param_values(&manifest);
    frame(&mut runtime, &mut events, 0.25, false, true);
    assert_all(&runtime, 0.575, 1.075);

    // Gate-off edges are absorbed without backlog; the live base is retained.
    manifest = manifest_with(
        &attached,
        "surface-peel-hit",
        &[("lift", 0.31), ("curl", 0.80), ("clip_trigger", 0.0)],
    );
    runtime.apply_param_values(&manifest);
    frame(&mut runtime, &mut events, 0.50, true, false);
    assert_all(&runtime, 0.31, 0.80);

    // Re-enabling and retriggering produces a fresh burst from the nonzero
    // event baseline, then returns to the currently held base.
    manifest = manifest_with(
        &attached,
        "surface-peel-hit",
        &[("lift", 0.31), ("curl", 0.80), ("clip_trigger", 1.0)],
    );
    runtime.apply_param_values(&manifest);
    frame(&mut runtime, &mut events, 0.75, true, true);
    assert_all(&runtime, 0.66, 1.15);
    frame(&mut runtime, &mut events, 1.25, false, true);
    assert_all(&runtime, 0.31, 0.80);

    // A freshly instantiated gate starts its output at zero even when the
    // retained owner event count is nonzero. Its envelope must use that same
    // zero baseline, so rebuilding does not manufacture a hit.
    runtime.clear_state();
    frame(&mut runtime, &mut events, 1.50, false, true);
    assert_all(&runtime, 0.31, 0.80);
    frame(&mut runtime, &mut events, 1.75, true, true);
    assert_all(&runtime, 0.66, 1.15);
}
