//! Every control the app ships resolves through the live param lookup
//! (SCENE_SURFACES_DESIGN.md D2 (one live lookup)): bundled presets and the
//! headless-UI fixtures, which are the scene projects the card is drawn from.

use manifold_core::effect_graph_def::BindingTarget;
use manifold_core::effects::PresetInstance;
use manifold_core::params::SpecSource;
use manifold_core::preset_def::PresetKind;
use manifold_core::preset_definition_registry as definitions;

const SCENE_FIXTURES: &[&str] = &[
    "gltfscene",
    "mushroomscene",
    "physicsscene",
    "physicsboxes",
    "fluidauthoring",
    "waterdambreak",
    "waterdambreakmatter",
    "inspector",
    "timeline",
];

/// Collects every control that failed to resolve, and every authored
/// control whose binding has a single node or modifier target (the lookup
/// should have described it).
fn audit(where_: &str, fx: &PresetInstance, misses: &mut Vec<String>) {
    let meta = fx.graph.as_ref().and_then(|g| g.preset_metadata.as_ref());
    for p in fx.params.iter() {
        let accidental_authored = p.source == SpecSource::Authored
            && p.origin == manifold_core::params::ParamOrigin::Bundled
            && meta.is_some_and(|m| {
                let mut targets = m.bindings.iter().filter(|b| b.id == p.id());
                matches!(
                    (targets.next(), targets.next()),
                    (Some(b), None) if !matches!(b.target, BindingTarget::Composite { .. })
                )
            });
        if p.source == SpecSource::Unresolved || accidental_authored {
            misses.push(format!(
                "{where_}: {} {:?} ({:?})",
                fx.effect_type().as_str(),
                p.id(),
                p.source
            ));
        }
    }
}

#[test]
fn every_bundled_and_fixture_param_resolves() {
    let mut misses = Vec::new();
    let mut checked = 0usize;
    for kind in [PresetKind::Generator, PresetKind::Effect] {
        for type_id in definitions::all_of_kind(kind) {
            let fx = definitions::create_default(&type_id);
            checked += fx.params.len();
            audit("bundled", &fx, &mut misses);
        }
    }
    let mut cache = super::fixtures::FixtureCache::default();
    for scene in SCENE_FIXTURES {
        let data = super::fixtures::build_cached(scene, &mut cache)
            .unwrap_or_else(|| panic!("unknown fixture {scene}"));
        for layer in &data.project.timeline.layers {
            for fx in layer.gen_params().into_iter().chain(layer.effects.iter().flatten()) {
                checked += fx.params.len();
                audit(scene, fx, &mut misses);
            }
        }
    }
    assert!(checked > 0, "no controls checked");
    assert!(
        misses.is_empty(),
        "{} of {checked} controls did not resolve through the lookup:\n{}",
        misses.len(),
        misses.join("\n")
    );
}

fn card_rows(project: &manifold_core::project::Project) -> Vec<String> {
    let mut rows = Vec::new();
    for layer in &project.timeline.layers {
        let Some(fx) = layer.gen_params() else { continue };
        for p in fx.params.iter().filter(|p| p.spec.card_visible) {
            rows.push(format!(
                "{} {} {:?} [{}, {}] {:?}",
                fx.effect_type().as_str(),
                p.id(),
                p.spec.name,
                p.spec.min,
                p.spec.max,
                p.spec.section
            ));
        }
    }
    rows
}

/// SCENE_SURFACES_DESIGN.md P1 gate: the card rows of the scene fixtures
/// before the join (the saved copies) and after a save and reload through
/// the lookup. Every differing line is a stale copy the join corrected.
#[test]
fn scene_card_rows_survive_save_and_reload() {
    let mut cache = super::fixtures::FixtureCache::default();
    for scene in ["physicsscene", "gltfscene"] {
        let data = super::fixtures::build_cached(scene, &mut cache)
            .unwrap_or_else(|| panic!("unknown fixture {scene}"));
        let before = card_rows(&data.project);
        let json = serde_json::to_string(&data.project).expect("save");
        let reloaded = manifold_io::loader::load_project_from_json(&json).expect("reload");
        let after = card_rows(&reloaded);
        let diff: Vec<_> = before
            .iter()
            .filter(|row| !after.contains(row))
            .chain(after.iter().filter(|row| !before.contains(row)))
            .collect();
        println!("CARD_DUMP {scene}: {} rows, {} differing", before.len(), diff.len());
        for line in &diff {
            println!("CARD_DIFF {scene}: {line}");
        }
        assert!(!before.is_empty(), "{scene} has no card rows");
        assert_eq!(before, after, "{scene} card rows changed across save and reload");
    }
}

/// A PhysicsSolids control the template declares, bound to one top-level node.
fn template_node_control() -> (manifold_core::effect_graph_def::EffectGraphDef, String, String) {
    let ty = manifold_core::PresetTypeId::from_string("PhysicsSolids".into());
    let template = manifold_nodes::bundled_presets::bundled_preset_def(&ty).expect("PhysicsSolids");
    let meta = template.preset_metadata.as_ref().unwrap();
    let (id, node_id) = meta
        .params
        .iter()
        .find_map(|s| {
            let mut bound = meta.bindings.iter().filter(|b| b.id == s.id);
            let (Some(b), None) = (bound.next(), bound.next()) else { return None };
            let BindingTarget::Node { node_id, .. } = &b.target else { return None };
            template
                .nodes
                .iter()
                .any(|n| &n.node_id == node_id)
                .then(|| (s.id.clone(), node_id.as_str().to_string()))
        })
        .expect("a template control bound to one node");
    (template.as_ref().clone(), id, node_id)
}

fn physics_solids(graph: manifold_core::effect_graph_def::EffectGraphDef) -> PresetInstance {
    let mut fx = PresetInstance::new_generator(manifold_core::PresetTypeId::from_string(
        "PhysicsSolids".into(),
    ));
    fx.init_defaults();
    fx.graph = Some(graph);
    fx.refresh_manifest_from_graph();
    fx
}

#[test]
fn template_answers_only_its_own_node_type() {
    let (mut graph, id, node_id) = template_node_control();
    let fx = physics_solids(graph.clone());
    assert_eq!(fx.params.get(&id).unwrap().source, SpecSource::Lookup);
    // The node was deleted and its id reused by another type.
    for node in &mut graph.nodes {
        if node.node_id.as_str() == node_id {
            node.type_id = "node.not_registered".into();
        }
    }
    let fx = physics_solids(graph);
    assert_eq!(fx.params.get(&id).unwrap().source, SpecSource::Unresolved);
}

#[test]
fn old_template_copy_section_becomes_a_choice_once() {
    let (mut graph, id, _) = template_node_control();
    for spec in &mut graph.preset_metadata.as_mut().unwrap().params {
        if spec.id == id {
            spec.section = Some("Old Section".into());
        }
    }
    let fx = physics_solids(graph);
    let p = fx.params.get(&id).unwrap();
    assert_eq!(p.choices.section, Some(Some("Old Section".into())));
    assert_eq!(p.spec.section.as_deref(), Some("Old Section"));
    // Save and reload: the choice rides the entry; the stub converts nothing new.
    let mut project = manifold_core::project::Project::default();
    let mut layer = manifold_core::layer::Layer::new_generator(
        "Physics Solids".into(),
        manifold_core::PresetTypeId::from_string("PhysicsSolids".into()),
        0,
    );
    *layer.gen_params_mut().unwrap() = fx.clone();
    project.timeline.layers.push(layer);
    let json = serde_json::to_string(&project).expect("save");
    let back = manifold_io::loader::load_project_from_json(&json).expect("reload");
    let q = back.timeline.layers[0].gen_params().unwrap().params.get(&id).unwrap().clone();
    let q = &q;
    assert_eq!((&q.choices, &q.spec), (&p.choices, &p.spec));
}
