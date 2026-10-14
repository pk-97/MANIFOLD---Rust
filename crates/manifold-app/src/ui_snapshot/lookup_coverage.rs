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
    // The command edits the graph; the manifest rebuild runs on the content
    // thread in the app, so do it here.
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

/// Each joined Lookup control that differs from its saved spec on what the
/// card shows: name, range and flags.
fn joined_vs_saved(fx: &PresetInstance) -> (usize, Vec<String>) {
    let Some(meta) = fx.graph.as_ref().and_then(|g| g.preset_metadata.as_ref()) else {
        return (0, Vec::new());
    };
    let close = |a: f32, b: f32| (a - b).abs() <= 1e-4 * a.abs().max(b.abs()).max(1.0);
    let mut compared = 0;
    let mut diffs = Vec::new();
    for p in fx.params.iter().filter(|p| p.source == SpecSource::Lookup) {
        let Some(saved) = meta.params.iter().find(|s| s.id == p.id()) else { continue };
        compared += 1;
        let j = &p.spec;
        let same = j.name == saved.name
            && close(j.min, saved.min)
            && close(j.max, saved.max)
            && (j.whole_numbers, j.is_toggle, j.is_trigger, j.card_visible, j.is_angle)
                == (saved.whole_numbers, saved.is_toggle, saved.is_trigger, saved.card_visible, saved.is_angle)
            && j.value_labels == saved.value_labels;
        if !same {
            diffs.push(format!(
                "{} {}: saved {:?} [{}, {}] w{} t{} g{} v{} a{} | joined {:?} [{}, {}] w{} t{} g{} v{} a{}",
                fx.effect_type().as_str(), p.id(),
                saved.name, saved.min, saved.max, saved.whole_numbers, saved.is_toggle,
                saved.is_trigger, saved.card_visible, saved.is_angle,
                j.name, j.min, j.max, j.whole_numbers, j.is_toggle, j.is_trigger, j.card_visible, j.is_angle
            ));
        }
    }
    (compared, diffs)
}

/// The card and panel dump on a scene with a model merged into its layer
/// graph through the production merge command.
fn merged_model_layer() -> PresetInstance {
    use manifold_core::GraphTarget;
    let mut project = manifold_core::project::Project::default();
    let ty = manifold_core::PresetTypeId::from_string("PhysicsSolids".into());
    let index = project.timeline.add_layer("Merged", manifold_core::types::LayerType::Generator, ty.clone());
    project.timeline.layers[index].gen_params_or_init();
    let layer = project.timeline.layers[index].layer_id.clone();
    let default = manifold_nodes::bundled_presets::bundled_preset_def(&ty).expect("PhysicsSolids").as_ref().clone();
    let glb = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/cc0__tiger_lily.glb");
    let plan = manifold_nodes_scene::node_graph::gltf_import::assemble_merge_plan(&default, &glb).expect("merge plan");
    let mut editing = manifold_editing::service::EditingService::new();
    editing.execute(
        Box::new(
            manifold_editing::commands::graph::ImportModelIntoSceneCommand::new(
                GraphTarget::Generator(layer), vec![], plan.render_scene_node_id, plan.new_nodes,
                plan.new_wires, plan.new_objects_count, plan.new_card_params.clone(), plan.new_card_bindings,
                plan.new_string_bindings, default,
            )
            .with_scene_bounds(plan.scene_bounds),
        ),
        &mut project,
    );
    assert_eq!(editing.take_rejection(), None);
    let fx = project.timeline.layers[index].gen_params().expect("merged layer").clone();
    let merged = plan.new_card_params.iter().filter(|s| fx.params.get(&s.id).is_some_and(|p| p.source == SpecSource::Lookup)).count();
    println!("JOIN_DUMP merged tiger_lily: {merged} of {} merged controls resolve through the lookup", plan.new_card_params.len());
    assert!(merged > 0, "merged controls missing from the joined manifest");
    fx
}

/// SCENE_SURFACES_DESIGN.md P1 gate (no visible change): for every control
/// the lookup describes, the joined name, range and flags equal the saved
/// full spec the stamp wrote.
#[test]
fn joined_specs_match_saved_specs_on_bundled_scenes() {
    let mut scenes: Vec<(String, PresetInstance)> = definitions::all_of_kind(PresetKind::Generator)
        .into_iter()
        .map(|t| with_graph(definitions::create_default(&t)))
        .filter(|fx| {
            fx.graph.as_ref().is_some_and(|g| g.nodes.iter().any(|n| n.type_id == "node.render_scene"))
        })
        .map(|fx| (format!("bundled {}", fx.effect_type().as_str()), fx))
        .collect();
    let mut cache = super::fixtures::FixtureCache::default();
    for scene in ["physicsscene", "gltfscene"] {
        let data = super::fixtures::build_cached(scene, &mut cache).expect("fixture");
        for fx in data.project.timeline.layers.iter().filter_map(|l| l.gen_params()) {
            scenes.push((format!("fixture {scene}"), with_graph(fx.clone())));
        }
    }
    scenes.push(("merged tiger_lily".into(), merged_model_layer()));
    let mut all = Vec::new();
    let mut total = 0;
    for (label, fx) in &scenes {
        let (compared, diffs) = joined_vs_saved(fx);
        total += compared;
        println!("JOIN_DUMP {label}: {compared} lookup controls, {} differing", diffs.len());
        all.extend(diffs.into_iter().map(|d| format!("{label}: {d}")));
    }
    assert!(total > 0, "no lookup controls compared");
    assert!(all.is_empty(), "{} joined specs differ from saved:\n{}", all.len(), all.join("\n"));
}

/// The instance with its graph made explicit: a layer on the catalog default
/// carries no graph, so join the bundled template the card is drawn from.
fn with_graph(mut fx: PresetInstance) -> PresetInstance {
    if fx.graph.is_none()
        && let Some(def) = manifold_nodes::bundled_presets::bundled_preset_def(fx.effect_type())
    {
        fx.graph = Some(def.as_ref().clone());
        fx.refresh_manifest_from_graph();
    }
    fx
}

/// D7: the drawer's rename and slider range on a lookup control are user
/// choices, so they survive save and reload, and undo and redo move them.
#[test]
fn drawer_rename_is_a_choice_and_survives_reload() {
    use manifold_core::GraphTarget;
    use manifold_editing::commands::effects::{BindingMappingEdit, EditParamMappingCommand};
    let mut cache = super::fixtures::FixtureCache::default();
    let mut project = super::fixtures::build_cached("physicsscene", &mut cache).expect("fixture").project.clone();
    let (index, id, name, range) = project
        .timeline
        .layers
        .iter()
        .enumerate()
        .find_map(|(i, l)| {
            let p = l.gen_params()?.params.iter().find(|p| p.source == SpecSource::Lookup)?;
            Some((i, p.id().to_string(), p.spec.name.clone(), (p.spec.min, p.spec.max)))
        })
        .expect("physicsscene has a lookup control");
    let layer = project.timeline.layers[index].layer_id.clone();
    let mut editing = manifold_editing::service::EditingService::new();
    let edit = BindingMappingEdit {
        label: Some("Lamp Power".into()),
        min: Some(range.0 + 0.5),
        max: Some(range.1 + 2.0),
        ..Default::default()
    };
    editing.execute(
        Box::new(EditParamMappingCommand::new(GraphTarget::Generator(layer), id.clone(), edit, None)),
        &mut project,
    );
    let control = |project: &manifold_core::project::Project| {
        let p = project.timeline.layers[index].gen_params().unwrap().params.get(&id).unwrap().clone();
        (p.spec.name.clone(), (p.spec.min, p.spec.max), p.choices.clone())
    };
    let reload = |project: &manifold_core::project::Project| {
        let json = serde_json::to_string(project).expect("save");
        manifold_io::loader::load_project_from_json(&json).expect("reload")
    };
    let edited = (String::from("Lamp Power"), (range.0 + 0.5, range.1 + 2.0));
    let after = control(&project);
    assert_eq!((after.0.clone(), after.1), edited);
    assert_eq!(after.2.rename.as_deref(), Some("Lamp Power"), "the rename is a choice");
    assert_eq!(after.2.slider, Some(edited.1), "the range is a choice");
    let reloaded = control(&reload(&project));
    assert_eq!((reloaded.0, reloaded.1), edited, "rename and range survive reload");

    assert!(editing.undo(&mut project));
    let undone = control(&project);
    assert_eq!((undone.0, undone.1), (name.clone(), range));
    let undone = control(&reload(&project));
    assert_eq!((undone.0, undone.1), (name, range), "undo leaves no choice behind");

    assert!(editing.redo(&mut project));
    let redone = control(&reload(&project));
    assert_eq!((redone.0, redone.1), edited, "redo survives reload");
}

/// INV-S14 at save level: a scene saves its described controls as id stubs
/// with unlabelled bindings, and a reload gives back the same manifest,
/// the user's choices included.
#[test]
fn saved_scene_has_id_only_lookup_entries() {
    let mut cache = super::fixtures::FixtureCache::default();
    for scene in ["physicsscene", "gltfscene"] {
        let mut project = super::fixtures::build_cached(scene, &mut cache).expect("fixture").project.clone();
        let mut chosen = 0;
        for layer in project.timeline.layers.iter_mut() {
            let Some(fx) = layer.gen_params_mut() else { continue };
            // A layer on the catalog default saves no graph; give it one so
            // its described controls reach the saved meta.params.
            *fx = with_graph(fx.clone());
            fx.folded_sections = vec!["Light".into()];
            if let Some(p) = fx.params.iter_mut().find(|p| p.source == SpecSource::Lookup) {
                p.choices.rename = Some("Chosen".into());
                p.spec.max += 1.0;
                p.choices.slider = Some((p.spec.min, p.spec.max));
                p.choices.clone().apply(&mut p.spec);
                chosen += 1;
            }
        }
        assert!(chosen > 0, "{scene} has no lookup control");
        let json = serde_json::to_string(&project).expect("save");
        let wire: serde_json::Value = serde_json::from_str(&json).unwrap();
        let reloaded = manifold_io::loader::load_project_from_json(&json).expect("reload");
        for (before, after) in project.timeline.layers.iter().zip(&reloaded.timeline.layers) {
            let (Some(a), Some(b)) = (before.gen_params(), after.gen_params()) else { continue };
            let view = |fx: &PresetInstance| {
                fx.params
                    .iter()
                    // Debug text: a NaN field never equals itself.
                    .map(|p| format!("{:?}", (p.id(), &p.spec, &p.choices, p.source, p.value)))
                    .collect::<Vec<_>>()
            };
            for (x, y) in view(a).into_iter().zip(view(b)) {
                assert_eq!(x, y, "{scene} control changed across save and reload");
            }
            assert_eq!(a.params.len(), b.params.len(), "{scene} control count changed");
            assert_eq!(a.folded_sections, b.folded_sections);
        }
        let mut lookup_entries = 0;
        let layers = wire["timeline"]["layers"].as_array().unwrap();
        for (layer, fx) in layers.iter().zip(project.timeline.layers.iter().map(|l| l.gen_params())) {
            let Some(fx) = fx else { continue };
            let meta = &layer["genParams"]["graph"]["presetMetadata"];
            for entry in meta["params"].as_array().into_iter().flatten() {
                let id = entry["id"].as_str().unwrap();
                if fx.params.get(id).is_some_and(|p| p.source == SpecSource::Lookup) {
                    lookup_entries += 1;
                    assert!(entry.get("min").is_none() && entry.get("max").is_none(), "{scene} {id} saved a copy: {entry}");
                }
            }
            for b in meta["bindings"].as_array().into_iter().flatten() {
                let id = b["id"].as_str().unwrap();
                if fx.params.get(id).is_some_and(|p| p.source == SpecSource::Lookup)
                    && b["target"]["kind"] != "composite"
                {
                    assert!(b.get("label").is_none(), "{scene} {id} saved a label: {b}");
                }
            }
        }
        assert!(lookup_entries > 0, "{scene} saved no lookup entries: {}", &json[..json.len().min(400)]);
    }
}
