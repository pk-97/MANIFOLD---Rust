#[test]
fn scene_force_projection_preserves_audio_and_uses_separate_picker() {
    let mut graph: EffectGraphDef = serde_json::from_str(manifold_nodes::testkit::assets::TESTS_FIXTURES_SCENE_MODIFIERS_NESTED_MULTIMATERIAL_V2_JSON).unwrap();
    let recipe = manifold_nodes::bundled_presets::bundled_preset_def(&manifold_core::PresetTypeId::new("RadialForce")).unwrap();
    let modifier = prepare_new_scene_modifier(&graph, recipe.as_ref(), "force".into(),
        SceneNodeRef { scope: vec![], node: "scan_render".into() }, SceneTargetSelection::AllObjects).unwrap();
    graph = manifold_core::scene_modifier_edit::insert_scene_modifier(&graph, 0, modifier).unwrap().graph;
    let mut host = PresetInstance::new_generator(manifold_core::PresetTypeId::new("PhotoscanBaseline"));
    host.graph = Some(graph.clone());
    host.refresh_manifest_from_graph();
    let strength = graph.preset_metadata.as_ref().unwrap().bindings.iter().find(|binding| matches!(
        &binding.target, manifold_core::effect_graph_def::BindingTarget::SceneModifier { param_id, .. } if param_id == "strength"
    )).unwrap().id.clone();
    host.audio_mods = Some(vec![ParameterAudioMod::new(strength.clone().into(), "drums".into(),
        AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full))]);
    let vm = SceneVm::from_def(&graph).unwrap();
    let surfaces = modifier_surfaces(&host, &graph, &vm, "layer", &[], (manifold_core::Bpm(120.0), 0.0));
    let surface = &surfaces[0];
    assert!(is_force_surface(surface, &graph));
    assert_eq!(surface.title, "Radial Force");
    assert!(!surface.rows.is_empty());
    for row in &surface.rows {
        assert!(host.params.get(row.id.as_ref()).is_some());
        assert_eq!(row.audio.active, row.id.as_ref() == strength);
    }
    let info = surface.modifier.as_ref().unwrap();
    assert!(info.targets_all);
    assert!(!info.objects.is_empty());
    assert!(info.objects.iter().all(|object| object.label.ends_with("(no physics)")));
    assert!(force_picker_entries(&graph, &vm).iter().any(|entry| entry.preset_id == "RadialForce" && entry.disabled.is_none()));
    assert!(modifier_picker_entries(&graph, &vm).iter().all(|entry| entry.preset_id != "RadialForce"));
    let mut project = manifold_core::project::Project::default();
    let mut layer = manifold_core::layer::Layer::new_generator("Scene".into(), host.generator_type().clone(), 0);
    layer.layer_id = manifold_core::LayerId::new("layer");
    *layer.gen_params_mut().unwrap() = host;
    project.timeline.layers.push(layer);
    let mut source = manifold_core::layer::Layer::new_trigger("Hits".into(), "layer".into(), 1);
    source.layer_id = "hits".into();
    project.timeline.layers.push(source);
    project.timeline.layers[0].gen_params_mut().unwrap().params.get_mut(&strength).unwrap()
        .clip_trigger_source = manifold_core::params::ClipTriggerSource::Lane { layer_id: "hits".into() };
    let mut projected = surfaces.clone();
    attach_project_sources(&mut projected, &project);
    let row = projected[0].rows.iter().find(|row| row.id.as_ref() == strength).unwrap();
    let route = row.clip_trigger.as_ref().expect("force card source selector");
    assert_eq!(route.target, manifold_ui::view::UiGraphTarget::Generator("layer".into()));
    assert_eq!(route.source_label, "Hits");
    let catalog = super::super::trigger_routing::TriggerRoutingCatalog::project(&project);
    assert_eq!(catalog.targets.iter().filter(|target| target.param_id.as_ref() == strength).count(), 1);
    assert!(super::super::trigger_routing::response_uses_scene_panel(&project,
        &manifold_core::GraphTarget::Generator("layer".into()), &strength));
    let saved = serde_json::to_vec(&project).unwrap();
    let mut loaded: manifold_core::project::Project = serde_json::from_slice(&saved).unwrap();
    assert_eq!(loaded.reconcile_param_manifests(), 0);
    let host = loaded.timeline.layers[0].gen_params().unwrap();
    let reloaded = modifier_surfaces(host, host.graph.as_ref().unwrap(), &vm, "layer", &[], (manifold_core::Bpm(120.0), 0.0));
    assert!(reloaded[0].rows.iter().find(|row| row.id.as_ref() == strength).unwrap().audio.active);
}

/// Force rows carry their manifest unit all the way to the readout text.
#[test]
fn radial_force_rows_format_with_their_units() {
    use manifold_ui::panels::param_slider_shared::format_param_value;
    let mut graph: EffectGraphDef = serde_json::from_str(manifold_nodes::testkit::assets::TESTS_FIXTURES_SCENE_MODIFIERS_NESTED_MULTIMATERIAL_V2_JSON).unwrap();
    let recipe = manifold_nodes::bundled_presets::bundled_preset_def(&manifold_core::PresetTypeId::new("RadialForce")).unwrap();
    let modifier = prepare_new_scene_modifier(&graph, recipe.as_ref(), "force".into(),
        SceneNodeRef { scope: vec![], node: "scan_render".into() }, SceneTargetSelection::AllObjects).unwrap();
    graph = manifold_core::scene_modifier_edit::insert_scene_modifier(&graph, 0, modifier).unwrap().graph;
    let mut host = PresetInstance::new_generator(manifold_core::PresetTypeId::new("PhotoscanBaseline"));
    host.graph = Some(graph.clone());
    host.refresh_manifest_from_graph();
    let vm = SceneVm::from_def(&graph).unwrap();
    let surfaces = modifier_surfaces(&host, &graph, &vm, "layer", &[], (manifold_core::Bpm(120.0), 0.0));
    let readout = |name: &str| {
        let row = surfaces[0].rows.iter().find(|row| row.spec.name == name).unwrap_or_else(|| panic!("row {name}"));
        let spec = &row.spec;
        format_param_value(row.value.effective, spec.min, spec.whole_numbers, spec.is_angle, spec.value_labels.as_deref(), spec.unit)
    };
    assert!(readout("Strength").ends_with(" m/s²"));
    assert!(readout("Impulse Size").ends_with(" m/s"));
}

/// A project with one Radial force on layer "layer", and the force's host
/// control ids for `fire`, `impulse_strength` and `strength`.
fn radial_force_project() -> (manifold_core::project::Project, [String; 3]) {
    let mut graph: EffectGraphDef = serde_json::from_str(manifold_nodes::testkit::assets::TESTS_FIXTURES_SCENE_MODIFIERS_NESTED_MULTIMATERIAL_V2_JSON).unwrap();
    let recipe = manifold_nodes::bundled_presets::bundled_preset_def(&manifold_core::PresetTypeId::new("RadialForce")).unwrap();
    let modifier = prepare_new_scene_modifier(&graph, recipe.as_ref(), "force".into(),
        SceneNodeRef { scope: vec![], node: "scan_render".into() }, SceneTargetSelection::AllObjects).unwrap();
    graph = manifold_core::scene_modifier_edit::insert_scene_modifier(&graph, 0, modifier).unwrap().graph;
    let host_id = |param: &str| graph.preset_metadata.as_ref().unwrap().bindings.iter().find(|binding| matches!(
        &binding.target, manifold_core::effect_graph_def::BindingTarget::SceneModifier { param_id, .. } if param_id == param
    )).unwrap().id.clone();
    let ids = [host_id("fire"), host_id("impulse_strength"), host_id("strength")];
    let mut host = PresetInstance::new_generator(manifold_core::PresetTypeId::new("PhotoscanBaseline"));
    host.graph = Some(graph);
    host.refresh_manifest_from_graph();
    let mut project = manifold_core::project::Project::default();
    let mut layer = manifold_core::layer::Layer::new_generator("Scene".into(), host.generator_type().clone(), 0);
    layer.layer_id = manifold_core::LayerId::new("layer");
    *layer.gen_params_mut().unwrap() = host;
    project.timeline.layers.push(layer);
    (project, ids)
}

/// Saves the project, writes `old` names into every saved copy of the
/// matching controls (host entry, host binding label, the recipe copy
/// inside the modifier; an empty local id leaves that copy alone) and
/// drops their tooltips, as a project saved before a relabel holds them;
/// then loads it.
fn reload_with_old_labels(project: &manifold_core::project::Project, old: &[(&str, &str, &str)]) -> manifold_core::project::Project {
    fn walk(value: &mut serde_json::Value, old: &[(&str, &str, &str)]) {
        match value {
            serde_json::Value::Object(map) => {
                let id = map.get("id").and_then(|id| id.as_str()).map(str::to_string);
                let matches = |id: &String, host: &str, local: &str| id == host || (!local.is_empty() && id == local);
                if let Some((_, _, name)) = id.and_then(|id| old.iter().find(|(host, local, _)| matches(&id, host, local))) {
                    map.remove("tooltip");
                    if map.contains_key("target") {
                        map.insert("label".into(), (*name).into());
                    } else if map.contains_key("defaultValue") {
                        map.insert("name".into(), (*name).into());
                    }
                }
                map.values_mut().for_each(|v| walk(v, old));
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(|v| walk(v, old)),
            _ => {}
        }
    }
    let mut saved = serde_json::to_value(project).unwrap();
    walk(&mut saved, old);
    let mut loaded: manifold_core::project::Project = serde_json::from_value(saved).unwrap();
    loaded.reconcile_param_manifests();
    loaded
}

/// D9 (one word list, on the declarations): a recipe relabel reaches a
/// force saved under the old words on its next load, with no migration
/// rung, and the stale words are not taken for a user rename.
#[test]
fn force_saved_with_old_labels_loads_with_the_recipe_labels() {
    let (project, [fire, impulse, strength]) = radial_force_project();
    let loaded = reload_with_old_labels(&project, &[
        (&fire, "fire", "Kick"),
        (&impulse, "impulse_strength", "Kick Size"),
        (&strength, "strength", "Strength"),
    ]);
    let host = loaded.timeline.layers[0].gen_params().unwrap();
    let param = |id: &str| host.params.get(id).unwrap();
    assert_eq!(param(&fire).spec.name, "Impulse");
    assert_eq!(param(&impulse).spec.name, "Impulse Size");
    assert_eq!(param(&strength).spec.tooltip.as_deref(), Some("Gravity is 9.8 m/s²"));
    for id in [&fire, &impulse, &strength] {
        assert_eq!(param(id).choices.rename, None, "{id}: a stale label is not a rename");
    }
    let graph = host.graph.as_ref().unwrap();
    let vm = SceneVm::from_def(graph).unwrap();
    let surfaces = modifier_surfaces(host, graph, &vm, "layer", &[], (manifold_core::Bpm(120.0), 0.0));
    let names: Vec<&str> = surfaces[0].rows.iter().map(|row| row.spec.name.as_str()).collect();
    assert!(names.contains(&"Impulse") && names.contains(&"Impulse Size"), "{names:?}");
    assert!(!names.iter().any(|name| name.starts_with("Kick")), "{names:?}");
}

/// A force row the user renamed keeps that name across save and load,
/// while a stale label on its sibling still gives way to the recipe's.
#[test]
fn renamed_force_row_keeps_its_name_across_save_and_load() {
    use manifold_editing::command::Command;
    use manifold_editing::commands::effects::{BindingMappingEdit, EditParamMappingCommand};
    let (mut project, [fire, impulse, _]) = radial_force_project();
    EditParamMappingCommand::new(manifold_core::GraphTarget::Generator("layer".into()), impulse.clone(),
        BindingMappingEdit { label: Some("Push".into()), ..Default::default() }, None).execute(&mut project);
    let loaded = reload_with_old_labels(&project, &[(&fire, "fire", "Kick")]);
    let host = loaded.timeline.layers[0].gen_params().unwrap();
    let renamed = host.params.get(&impulse).unwrap();
    assert_eq!(renamed.spec.name, "Push");
    assert_eq!(renamed.choices.rename.as_deref(), Some("Push"));
    assert_eq!(host.params.get(&fire).unwrap().spec.name, "Impulse");
    let resaved = reload_with_old_labels(&loaded, &[]);
    assert_eq!(resaved.timeline.layers[0].gen_params().unwrap().params.get(&impulse).unwrap().spec.name, "Push");
}

/// Before the drawer wrote rename choices, a rename went straight into
/// the host's saved name. On load that name becomes a rename choice when
/// the recipe never shipped it; a shipped old label does not. Only the
/// file read converts: an in-memory rebuild never revives a rename the
/// user has since taken back.
#[test]
fn force_renamed_into_its_saved_name_keeps_the_rename() {
    let (project, [fire, impulse, _]) = radial_force_project();
    let mut loaded = reload_with_old_labels(&project, &[(&impulse, "", "Push"), (&fire, "fire", "Kick")]);
    let host = loaded.timeline.layers[0].gen_params_mut().unwrap();
    assert_eq!(host.params.get(&impulse).unwrap().spec.name, "Push");
    assert_eq!(host.params.get(&impulse).unwrap().choices.rename.as_deref(), Some("Push"));
    assert_eq!(host.params.get(&fire).unwrap().spec.name, "Impulse");
    assert_eq!(host.params.get(&fire).unwrap().choices.rename, None);
    host.refresh_manifest_from_graph();
    assert_eq!(host.params.get(&impulse).unwrap().spec.name, "Push");
    let resaved = reload_with_old_labels(&loaded, &[]);
    assert_eq!(resaved.timeline.layers[0].gen_params().unwrap().params.get(&impulse).unwrap().spec.name, "Push");

    let host = loaded.timeline.layers[0].gen_params_mut().unwrap();
    let taken_back = host.params.get_mut(&impulse).unwrap();
    taken_back.choices.rename = None;
    taken_back.spec.name = "Impulse Size".into();
    host.refresh_manifest_from_graph();
    assert_eq!(host.params.get(&impulse).unwrap().spec.name, "Impulse Size");
    assert_eq!(host.params.get(&impulse).unwrap().choices.rename, None);
}

/// A force card's audio-mod meter shows the level the content thread
/// captured for that mod. The mod lives on the generator under the host
/// binding id, so the card keys its meter on the generator instance, never
/// on its own `scene_modifier:` card identity.
#[test]
fn force_card_audio_meter_resolves_in_the_producer_capture() {
    use manifold_core::audio_trigger::{FireMeterCapture, fire_meter_key_for_param};
    let mut graph: EffectGraphDef = serde_json::from_str(manifold_nodes::testkit::assets::TESTS_FIXTURES_SCENE_MODIFIERS_NESTED_MULTIMATERIAL_V2_JSON).unwrap();
    let recipe = manifold_nodes::bundled_presets::bundled_preset_def(&manifold_core::PresetTypeId::new("RadialForce")).unwrap();
    let modifier = prepare_new_scene_modifier(&graph, recipe.as_ref(), "force".into(),
        SceneNodeRef { scope: vec![], node: "scan_render".into() }, SceneTargetSelection::AllObjects).unwrap();
    graph = manifold_core::scene_modifier_edit::insert_scene_modifier(&graph, 0, modifier).unwrap().graph;
    let mut host = PresetInstance::new_generator(manifold_core::PresetTypeId::new("PhotoscanBaseline"));
    host.graph = Some(graph.clone());
    host.refresh_manifest_from_graph();
    let strength = graph.preset_metadata.as_ref().unwrap().bindings.iter().find(|binding| matches!(
        &binding.target, manifold_core::effect_graph_def::BindingTarget::SceneModifier { param_id, .. } if param_id == "strength"
    )).unwrap().id.clone();
    let send = manifold_core::audio_setup::AudioSend::new("Drums");
    let mut kick = ParameterAudioMod::new(strength.clone().into(), send.id.clone(),
        AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full));
    kick.shape.attack_ms = 0.0;
    kick.shape.release_ms = 0.0;
    host.audio_mods = Some(vec![kick]);
    let mut project = manifold_core::project::Project::default();
    project.audio_setup.sends.push(send);
    let mut layer = manifold_core::layer::Layer::new_generator("Scene".into(), host.generator_type().clone(), 0);
    *layer.gen_params_mut().unwrap() = host;
    project.timeline.layers.push(layer);

    let mut bands = [manifold_core::BandFeatures::default(); 4];
    bands[AudioBand::Full.index()].amplitude = 0.8;
    let snapshot = manifold_core::AudioFeatureSnapshot {
        sends: vec![manifold_core::SendFeatures { bands, ..Default::default() }],
        ..Default::default()
    };
    let mut fire_meters = FireMeterCapture::default();
    manifold_playback::modulation::evaluate_all_audio_mods(
        &mut project, &snapshot, manifold_core::Seconds(1.0 / 60.0),
        manifold_core::Seconds::ZERO,
        &manifold_playback::clip_controls::ClipControlFrame::default(),
        &mut Vec::<manifold_playback::modulation::TriggerPulse>::new(), &mut fire_meters,
    );
    let host = project.timeline.layers[0].gen_params().unwrap();
    let produced = fire_meter_key_for_param(host.id.as_str(), &strength);
    assert!(fire_meters.get(produced).is_some_and(|level| level > 0.0), "the content thread meters the force card's kick");

    let vm = SceneVm::from_def(&graph).unwrap();
    let surfaces = modifier_surfaces(host, &graph, &vm, "layer", &[], (manifold_core::Bpm(120.0), 0.0));
    let mut tree = manifold_ui::UITree::new();
    let rect = manifold_ui::Rect::new(0.0, 0.0, 320.0, 2000.0);
    let region = tree.begin_region(rect, manifold_ui::ZTier::Base, "force_card", manifold_ui::UIFlags::empty());
    let content_start = tree.count();
    let mut card = manifold_ui::panels::param_card::ParamCardPanel::new();
    card.configure(&surfaces[0]);
    card.build(&mut tree, rect);
    tree.end_region(region, content_start);
    let requested = std::cell::RefCell::new(Vec::new());
    card.update_fire_meters(&mut tree, &|key| {
        requested.borrow_mut().push(key);
        fire_meters.get(key)
    }, 1.0 / 60.0);
    assert_eq!(requested.into_inner(), vec![produced], "the card's kick meter reads the key the content thread fills");
}

use super::*;
use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use manifold_nodes_scene::node_graph::{scene_modifier_authoring::prepare_new_scene_modifier, scene_vm::SceneVm};

#[test]
fn modifier_surfaces_keep_audio_on_its_parameter_after_filtering_and_stack_reorder() {
    let mut graph: EffectGraphDef = serde_json::from_str(manifold_nodes::testkit::assets::TESTS_FIXTURES_SCENE_MODIFIERS_NESTED_MULTIMATERIAL_V2_JSON).unwrap();
    for preset in ["Wind", "SceneFog"] {
        let recipe = manifold_nodes::bundled_presets::bundled_preset_def(
            &manifold_core::PresetTypeId::new(preset),
        ).unwrap();
        let modifier = prepare_new_scene_modifier(
            &graph, recipe.as_ref(), preset.into(),
            SceneNodeRef { scope: vec![], node: "scan_render".into() },
            SceneTargetSelection::AllObjects,
        ).unwrap();
        graph = manifold_core::scene_modifier_edit::insert_scene_modifier(
            &graph, graph.scene_modifiers.len(), modifier,
        ).unwrap().graph;
    }
    let mut gp = PresetInstance::new_generator(manifold_core::PresetTypeId::new("PhotoscanBaseline"));
    gp.graph = Some(graph.clone());
    gp.refresh_manifest_from_graph();
    // Match the reported collision: generator audio at full row 4,
    // while Wind's local row 4 is Turbulence.
    let prefix = (0..5).map(|i| {
        let mut spec = gp.params.iter().next().unwrap().spec.clone();
        spec.id = format!("host_{i}");
        Param::user_added(spec)
    });
    gp.params = ParamManifest::from_params(prefix.chain(gp.params.iter().cloned()).collect());
    let make_mod = |id: &str| ParameterAudioMod::new(
        id.to_string().into(), "host".into(),
        AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
    );
    let mut host_mod = make_mod("host_4");
    host_mod.shape.release_ms = 63.616074;
    gp.audio_mods = Some(vec![host_mod]);
    // Hidden enable rows can have bindings too; their audio must never
    // leak into a visible parameter when the card drops them.
    for binding in &graph.preset_metadata.as_ref().unwrap().bindings {
        if let manifold_core::effect_graph_def::BindingTarget::SceneModifier { param_id, .. } = &binding.target
            && param_id == "enabled"
        {
            gp.audio_mods.as_mut().unwrap().push(make_mod(&binding.id));
        }
    }
    let vm = SceneVm::from_def(&graph).unwrap();
    let surfaces = modifier_surfaces(&gp, &graph, &vm, "layer", &[], (manifold_core::Bpm(120.0), 0.0));
    assert_eq!(surfaces.len(), 2);
    assert_eq!(surfaces[0].rows[4].spec.name, "Turbulence");
    for surface in &surfaces {
        assert!(surface.rows.iter().all(|row| !row.audio.active),
            "generator audio must not appear on {}", surface.title);
    }
    let render_param = surfaces[0].rows[4].id.clone();
    let fog_param = surfaces[1].rows[0].id.clone();
    let mut disabled_mod = make_mod(&surfaces[0].rows[0].id);
    disabled_mod.enabled = false;
    gp.audio_mods.as_mut().unwrap().push(disabled_mod);
    let mut render_mod = make_mod(&render_param);
    render_mod.shape.release_ms = 250.0;
    render_mod.source.send_id = "render".into();
    render_mod.shape.range_min = 0.2;
    render_mod.shape.range_max = 0.8;
    let mut fog_mod = make_mod(&fog_param);
    fog_mod.source.send_id = "fog".into();
    fog_mod.shape.range_min = 0.35;
    fog_mod.shape.range_max = 0.65;
    gp.audio_mods.as_mut().unwrap().extend([render_mod, fog_mod]);
    let mut setup = manifold_core::audio_setup::AudioSetup::default();
    let mut host_send = manifold_core::audio_setup::AudioSend::new("Host");
    host_send.id = "host".into();
    let mut render_send = manifold_core::audio_setup::AudioSend::new("Render");
    render_send.id = "render".into();
    let mut fog_send = manifold_core::audio_setup::AudioSend::new("Fog");
    fog_send.id = "fog".into();
    setup.sends = vec![host_send, render_send, fog_send];
    for pass in 0..2 {
        let mut surfaces = modifier_surfaces(&gp, &graph, &vm, "layer", &[], (manifold_core::Bpm(120.0), 0.0));
        attach_audio_sends(&mut surfaces, &setup);
        for (index, surface) in surfaces.iter().enumerate() {
            assert_eq!(surface.audio_sends.len(), 3);
            let send_order: Vec<&str> = surface.audio_sends.iter().map(|send| send.id.as_str()).collect();
            assert_eq!(send_order, if pass == 0 {
                vec!["host", "render", "fog"]
            } else {
                vec!["fog", "render", "host"]
            });
            let recipe = graph.scene_modifiers[index]
                .graph
                .preset_metadata
                .as_ref()
                .unwrap()
                .scene_modifier
                .as_ref()
                .unwrap();
            assert!(surface.rows.iter().all(|row| {
                let binding = graph.preset_metadata.as_ref().unwrap().bindings.iter()
                    .find(|binding| binding.id == row.id.as_ref()).unwrap();
                let manifold_core::effect_graph_def::BindingTarget::SceneModifier { param_id, .. } = &binding.target
                    else { panic!("modifier row must have a modifier binding") };
                param_id != &recipe.enabled_param
                    && !recipe.preparation_params.contains(param_id)
            }));
            for row in &surface.rows {
                let audio = &row.audio;
                assert_eq!(audio.active, row.id == render_param || row.id == fog_param);
                if row.id == render_param {
                    assert_eq!(audio.release_ms, 250.0);
                    assert_eq!(audio.send_id.as_ref().map(|id| id.as_str()), Some("render"));
                    assert_eq!((audio.range_min, audio.range_max), (0.2, 0.8));
                } else if row.id == fog_param {
                    assert_eq!(audio.send_id.as_ref().map(|id| id.as_str()), Some("fog"));
                    assert_eq!((audio.range_min, audio.range_max), (0.35, 0.65));
                }
            }
        }
        if pass == 0 {
            setup.sends.reverse();
        }
        graph.scene_modifiers.reverse();
        gp.graph = Some(graph.clone());
    }
}

#[test]
fn prepared_enabled_parameter_projects_from_local_default_without_host_binding() {
    let recipe = manifold_nodes::bundled_presets::bundled_preset_def(
        &manifold_core::PresetTypeId::new("Shatter"),
    )
    .unwrap().as_ref().clone();
    let mut instance = manifold_core::scene_modifier_preset::SceneModifierInstanceDef {
        id: "shatter".into(),
        scene: SceneNodeRef { scope: vec![], node: "scene".into() },
        targets: SceneTargetSelection::AllObjects,
        mesh_frames: Vec::new(),
        legacy_math_view_carrier: None,
        graph: Box::new(recipe),
    };
    let host = PresetInstance::new_generator(manifold_core::PresetTypeId::new("host"));
    assert!(modifier_enabled_value_for_binding(&host, &[], &instance, "enabled"));
    instance.graph.preset_metadata.as_mut().unwrap().params.iter_mut()
        .find(|param| param.id == "enabled").unwrap().default_value = 0.0;
    assert!(!modifier_enabled_value_for_binding(&host, &[], &instance, "enabled"));
}
