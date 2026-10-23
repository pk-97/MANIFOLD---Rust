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
    "fluidauthoring",
    "waterdambreak",
    "waterdambreakmatter",
    "oceanscene",
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
    manifold_nodes::engine_scenes::install();
    let ty = manifold_core::PresetTypeId::from_string("PhysicsSolids".into());
    let template = manifold_nodes::engine_scenes::scene_def(&ty).expect("PhysicsSolids");
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
    manifold_nodes::engine_scenes::install();
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
fn old_template_copy_section_is_ignored_and_the_template_wins() {
    // D2 (one live lookup), old projects: a saved section that differs from
    // the lookup is an old copy, ignored and never converted into a choice,
    // so a later section fix is never read as a user choice.
    let (mut graph, id, _) = template_node_control();
    let template_section = graph.preset_metadata.as_ref().unwrap().params.iter()
        .find(|spec| spec.id == id).unwrap().section.clone();
    assert_ne!(template_section.as_deref(), Some("Old Section"));
    for spec in &mut graph.preset_metadata.as_mut().unwrap().params {
        if spec.id == id {
            spec.section = Some("Old Section".into());
        }
    }
    let fx = physics_solids(graph);
    let p = fx.params.get(&id).unwrap();
    assert_eq!(p.choices.section, None);
    assert_eq!(p.spec.section, template_section);
    // Save and reload: nothing is converted later either.
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
        (p.spec.name.clone(), p.slider_span(), p.choices.clone())
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
                p.choices.slider = Some((p.spec.min, p.spec.max + 1.0));
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
            // D24: only a control whose name the lookup supplies saves without
            // one; every other control keeps its authored label.
            let graph = fx.graph.as_ref().unwrap();
            let declared = manifold_core::param_lookup::Declared::new(graph).unwrap();
            for b in meta["bindings"].as_array().into_iter().flatten() {
                let id = b["id"].as_str().unwrap();
                if fx.params.get(id).is_some_and(|p| p.source == SpecSource::Lookup)
                    && b["target"]["kind"] != "composite"
                {
                    let authored = graph.preset_metadata.as_ref().unwrap().bindings.iter()
                        .find(|x| x.id == id).is_some_and(|x| !x.label.is_empty());
                    if declared.lookup_names(id) {
                        assert!(b.get("label").is_none(), "{scene} {id} saved a label: {b}");
                    } else if authored {
                        assert!(b.get("label").is_some(), "{scene} {id} lost its authored label: {b}");
                    }
                }
            }
        }
        assert!(lookup_entries > 0, "{scene} saved no lookup entries: {}", &json[..json.len().min(400)]);
    }
}

/// SCENE_SURFACES_DESIGN.md P8 round trip: a slider range set on a card row
/// and on a Scene tab row holds across save and reload, and the declared
/// range under it never moves.
#[test]
fn slider_range_round_trips_on_card_and_panel_rows() {
    use manifold_core::scene_surface::SurfaceRole;
    use manifold_editing::command::Command;
    let mut cache = super::fixtures::FixtureCache::default();
    let mut project = super::fixtures::build_cached("gltfscene", &mut cache).expect("fixture").project.clone();
    let (index, layer) = project
        .timeline
        .layers
        .iter()
        .enumerate()
        .find(|(_, l)| l.gen_params().is_some_and(|g| g.params.iter().any(|p| p.spec.scene.is_some())))
        .map(|(i, l)| (i, l.layer_id.clone()))
        .expect("gltfscene has a scene layer");
    let pick = |role: SurfaceRole| {
        project.timeline.layers[index].gen_params().unwrap().params.iter()
            .find(|p| p.spec.scene.is_some_and(|s| s.role == role) && !p.spec.is_toggle && !p.spec.is_trigger
                && p.spec.value_labels.is_empty() && p.spec.max > p.spec.min)
            .map(|p| (p.id().to_string(), (p.spec.min, p.spec.max)))
            .expect("a slider row in that role")
    };
    let (card, card_declared) = pick(SurfaceRole::Performance);
    let (panel, panel_declared) = pick(SurfaceRole::Setup);
    for (id, (lo, hi)) in [(&card, card_declared), (&panel, panel_declared)] {
        let range = Some((lo - 1.0, hi + 3.0));
        manifold_editing::commands::graph::SetSliderRangeCommand::new(
            manifold_core::GraphTarget::Generator(layer.clone()),
            id.clone(),
            range,
        )
        .execute(&mut project);
    }
    let json = serde_json::to_string(&project).expect("save");
    let reloaded = manifold_io::loader::load_project_from_json(&json).expect("reload");
    let params = &reloaded.timeline.layers[index].gen_params().unwrap().params;
    for (id, (lo, hi)) in [(&card, card_declared), (&panel, panel_declared)] {
        let p = params.get(id).unwrap();
        assert_eq!(p.slider_span(), (lo - 1.0, hi + 3.0), "{id} slider range holds");
        assert_eq!((p.spec.min, p.spec.max), (lo, hi), "{id} declared range never moves");
    }
}

/// SCENE_SURFACES_DESIGN.md P5 round trip: a gltfscene project saved with
/// the old labels written into its controls loads with the new labels and
/// units, with no migration, and every OSC address still resolves to the
/// same control.
#[test]
fn old_labels_load_as_new_names_with_addresses_kept() {
    use manifold_core::scene_surface::Unit;
    const RENAMED: [(&str, &str, &str); 3] = [
        ("_fov_y", "FOV Y", "Field of View"),
        ("_look_y", "Look Y", "Target Height"),
        ("_exposure_ev", "Exposure (EV)", "Exposure"),
    ];
    let mut cache = super::fixtures::FixtureCache::default();
    let mut project = super::fixtures::build_cached("gltfscene", &mut cache).expect("fixture").project.clone();
    for layer in project.timeline.layers.iter_mut() {
        if let Some(fx) = layer.gen_params_mut() {
            *fx = with_graph(fx.clone());
        }
    }
    let addresses = |project: &manifold_core::project::Project| {
        let mut out = Vec::new();
        for layer in &project.timeline.layers {
            let Some(fx) = layer.gen_params() else { continue };
            for (suffix, _, _) in RENAMED {
                for p in fx.params.iter().filter(|p| p.id().ends_with(suffix)) {
                    let address = definitions::get_osc_address_for_layer_by_id(fx.effect_type(), layer.layer_id.as_str(), p.id())
                        .unwrap_or_else(|| panic!("{} has no OSC address", p.id()));
                    out.push((address, p.spec.name.clone(), p.spec.unit));
                }
            }
        }
        out
    };
    let before = addresses(&project);

    let mut wire: serde_json::Value = serde_json::to_value(&project).expect("save");
    let mut stale = 0;
    for layer in wire["timeline"]["layers"].as_array_mut().unwrap() {
        let meta = &mut layer["genParams"]["graph"]["presetMetadata"];
        for (list, key) in [("params", "name"), ("bindings", "label")] {
            for entry in meta[list].as_array_mut().into_iter().flatten() {
                let id = entry["id"].as_str().unwrap_or_default().to_owned();
                if let Some((_, old, _)) = RENAMED.iter().find(|(suffix, _, _)| id.ends_with(suffix)) {
                    entry[key] = serde_json::json!(old);
                    stale += 1;
                }
            }
        }
    }
    assert!(stale >= 6, "the fixture lost a camera or lens control: {stale} stale copies written");
    let reloaded = manifold_io::loader::load_project_from_json(&wire.to_string()).expect("reload");
    let after = addresses(&reloaded);

    assert_eq!(after.len(), RENAMED.len(), "one control per renamed param: {after:?}");
    for (suffix, old, new) in RENAMED {
        let (address, name, unit) = after.iter().find(|(a, ..)| a.ends_with(suffix)).expect("control kept");
        assert_eq!(name, new, "{old} reads as {new} after reload");
        assert!(before.iter().any(|(a, ..)| a == address), "{address} moved");
        if suffix == "_exposure_ev" {
            assert_eq!(*unit, Some(Unit::Ev), "Exposure carries its unit");
        }
    }
}
