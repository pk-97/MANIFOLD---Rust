//! The scene card: a scene generator's performance surface
//! (SCENE_SURFACES_DESIGN.md section 3.6 (card projection), D4 (card layout)).
//!
//! Rows stay ordinary manifest rows with their real ids, values, modulation
//! and mappings. This module only chooses, names, sections and orders them,
//! from two facts: each control's scene description (the joined
//! `Param.spec.scene`) and the scene item that owns it (`owner_of`, the one
//! ownership rule the Scene tab also uses). It never reads a node type or a
//! param name: every per-type fact is a row of the scene table or an
//! authored `scene` field.

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::effects::PresetInstance;
use manifold_core::params::{Param, ParamOrigin};
use manifold_core::scene_modifier_preset::SceneTargetSelection;
use manifold_core::scene_surface::{CardBand, CardLabel, CardPin, Feature, Gate, SceneItemKind, SurfaceRole};
use manifold_nodes_scene::node_graph::scene_items::{SceneItem, SceneItemId, owner_of, scene_items};
use manifold_nodes_scene::node_graph::scene_vm::SceneVm;
use manifold_ui::param_surface::{ColourGroup, ParamRow, RgbMembers};

const NO_PHYSICS_TARGETS: &str = "No physics objects in this force's targets";

/// Curate a scene generator's rows into the card's bands (Camera, Scene,
/// Forces, Lighting, Look). `false` means `def` is not a live scene graph
/// and `rows` was left untouched.
pub(super) fn scene_card_rows(rows: &mut Vec<ParamRow>, inst: &PresetInstance, def: &EffectGraphDef) -> bool {
    let Some(vm) = SceneVm::from_def(def) else {
        return false;
    };
    let items = scene_items(def, &vm);
    let meta = def.preset_metadata.as_ref();
    let authored_title = meta.map(|meta| meta.display_name.clone()).unwrap_or_default();
    let owner = |id: &str| meta.and_then(|meta| owner_of(&items, meta, id));
    let switches = FeatureSwitches::read(rows, inst, &owner);
    let idle_forces = idle_forces(def, &items);
    let colours = item_colours(rows, inst, def, &items, &owner);

    let mut placed: Vec<(SortKey, ParamRow)> = Vec::with_capacity(rows.len());
    for (manifest_index, mut row) in std::mem::take(rows).into_iter().enumerate() {
        let Some(param) = inst.params.get(row.id.as_ref()) else { continue };
        let owner_index = owner(row.id.as_ref());
        let item = owner_index.map(|index| &items[index]);
        // A pinned row stays whatever its gate: Show on Card and every
        // modulator attach write the pin (D6 (attached rows)).
        let pinned = param.choices.pin == Some(CardPin::Shown);
        let id = row.id.as_ref();
        let Some(place) = Placement::of(param, item, user_added(param, def, id), modifier_bound(def, id)) else {
            continue;
        };
        if !pinned && !row.value.exposed {
            continue;
        }
        match place.gate {
            Some(Gate::Fold(feature)) if !pinned && !switches.is_on(feature) => continue,
            // A pinned row stays with its feature off, dimmed like a Dim row.
            Some(Gate::Fold(feature) | Gate::Dim(feature)) if !switches.is_on(feature) => {
                row.spec.inactive_reason = Some(feature.off_reason().into());
            }
            _ => {}
        }
        if item.is_some_and(|item| matches!(&item.id, SceneItemId::Force(id) if idle_forces.contains(id))) {
            row.spec.inactive_reason = Some(NO_PHYSICS_TARGETS.into());
        }
        if place.label == CardLabel::ItemName
            && let Some(index) = owner_index
        {
            row.rgb_members = colours.iter().find(|(item, _)| *item == index).map(|(_, members)| members.clone());
        }
        if param.choices.rename.is_none() {
            match (place.label, item) {
                (CardLabel::ItemName, Some(item)) => row.spec.name = item.name.clone(),
                (CardLabel::GroupName, _) => {
                    if let Some(scene) = param.spec.scene {
                        row.spec.name = scene.group.title().into();
                    }
                }
                _ => {}
            }
        }
        // One section per item in the Scene and Forces bands; the rest share
        // their band's title. Authored rows sort after every item.
        let per_item = matches!(place.band, CardBand::Scene | CardBand::Forces);
        row.spec.section = Some(match (per_item, item) {
            (true, Some(item)) => item.name.clone(),
            (true, None) if place.band == CardBand::Scene => authored_title.clone(),
            _ => place.band.title().into(),
        });
        let item_rank = owner_index.unwrap_or(items.len());
        let key = if per_item {
            (place.band, item_rank, place.card_order, 0, manifest_index)
        } else {
            (place.band, 0, place.card_order, item_rank, manifest_index)
        };
        placed.push((key, row));
    }
    placed.sort_by_key(|(key, _)| *key);
    *rows = placed.into_iter().map(|(_, row)| row).collect();
    true
}

/// Lighting starts folded past this many light rows (D11 (lights)).
const LIGHT_ROWS_BEFORE_FOLD: usize = 8;

/// The card's folded section titles, from curated `rows`: the user's saved
/// fold choices (D7 (user choices)) over the default, which folds Lighting
/// past eight light rows. `None` when `def` is not a live scene graph.
pub(super) fn folded_sections(inst: &PresetInstance, def: &EffectGraphDef, rows: &[ParamRow]) -> Option<Vec<String>> {
    let vm = SceneVm::from_def(def)?;
    let items = scene_items(def, &vm);
    let meta = def.preset_metadata.as_ref();
    let lighting = CardBand::Lighting.title();
    let light_rows = rows
        .iter()
        .filter(|row| row.spec.section.as_deref() == Some(lighting))
        .filter(|row| {
            meta.and_then(|meta| owner_of(&items, meta, row.id.as_ref()))
                .is_some_and(|index| items[index].kind == SceneItemKind::Light)
        })
        .count();
    let mut titles: Vec<&str> = rows.iter().filter_map(|row| row.spec.section.as_deref()).collect();
    titles.dedup();
    let saved = |list: &[String], title: &str| list.iter().any(|saved| saved == title);
    Some(
        titles
            .into_iter()
            .filter(|title| {
                saved(&inst.folded_sections, title)
                    || (!saved(&inst.unfolded_sections, title) && *title == lighting && light_rows > LIGHT_ROWS_BEFORE_FOLD)
            })
            .map(str::to_string)
            .collect(),
    )
}

/// (band, section rank, card order, item rank, manifest order). A per-item
/// band ranks its sections by item; a shared band orders by card order
/// first, so Lighting reads Exposure, Environment, then each light.
type SortKey = (CardBand, usize, u8, usize, usize);

/// Where one row goes on the card, or `None` when it stays off.
struct Placement {
    band: CardBand,
    card_order: u8,
    label: CardLabel,
    gate: Option<Gate>,
}

impl Placement {
    fn of(param: &Param, item: Option<&SceneItem>, user_added: bool, modifier_bound: bool) -> Option<Self> {
        let pin = param.choices.pin;
        let shown = pin == Some(CardPin::Shown) || (user_added && pin != Some(CardPin::Removed));
        let Some(scene) = param.spec.scene else {
            // An authored control (composite macro, preset control on a
            // non-scene node) follows its card visibility, in the Scene band.
            // A scene modifier's control with no scene description already
            // has its home on the modifier's own card; only recipes that
            // author scene fields (the forces) also place rows here.
            let on = shown || (!modifier_bound && param.spec.card_visible && pin != Some(CardPin::Removed));
            return on.then_some(Self { band: CardBand::Scene, card_order: 0, label: CardLabel::Param, gate: None });
        };
        let performance = scene.role == SurfaceRole::Performance && pin != Some(CardPin::Removed);
        // A scene control no item owns drives nothing on screen (an unwired
        // camera's orbit); it builds no row unless the user put it there.
        if !(shown || (performance && item.is_some())) {
            return None;
        }
        let band = scene.band.filter(|_| scene.role == SurfaceRole::Performance).unwrap_or_else(|| item_band(item));
        Some(Self { band, card_order: scene.card_order, label: scene.card_label, gate: scene.gate })
    }
}

/// The band an item's off-role rows join when they are shown anyway.
fn item_band(item: Option<&SceneItem>) -> CardBand {
    match item.map(|item| item.kind) {
        Some(SceneItemKind::Camera) => CardBand::Camera,
        Some(SceneItemKind::Light) => CardBand::Lighting,
        Some(SceneItemKind::Force) => CardBand::Forces,
        _ => CardBand::Scene,
    }
}

/// Each feature's switch, read from the authored base value (D12 (row sets
/// follow authored values)): a driver flickering the effective value never
/// rebuilds the card. A feature with no owned switch is off.
struct FeatureSwitches {
    on: Vec<Feature>,
}

impl FeatureSwitches {
    fn read(rows: &[ParamRow], inst: &PresetInstance, owner: &dyn Fn(&str) -> Option<usize>) -> Self {
        let on = rows
            .iter()
            .filter_map(|row| {
                let param = inst.params.get(row.id.as_ref())?;
                let feature = param.spec.scene?.switch?;
                (owner(row.id.as_ref()).is_some() && param.base > 0.5).then_some(feature)
            })
            .collect();
        Self { on }
    }

    fn is_on(&self, feature: Feature) -> bool {
        self.on.contains(&feature)
    }
}

/// Each light's colour swatch, from the rows its item owns whose scene
/// description marks one colour channel. The row the card names after the
/// item carries it, so recolouring a light is one gesture on its fader. An
/// item with more than one colour, or channels a single RGB write cannot
/// own, gets no swatch.
fn item_colours(
    rows: &[ParamRow],
    inst: &PresetInstance,
    def: &EffectGraphDef,
    items: &[SceneItem],
    owner: &dyn Fn(&str) -> Option<usize>,
) -> Vec<(usize, RgbMembers)> {
    let mut channels: Vec<(usize, [Option<&ParamRow>; 3], bool)> = Vec::new();
    for row in rows {
        let Some(channel) = inst.params.get(row.id.as_ref()).and_then(|p| p.spec.scene).and_then(|s| s.colour) else {
            continue;
        };
        let Some(item) = owner(row.id.as_ref()).filter(|&i| items[i].kind == SceneItemKind::Light) else {
            continue;
        };
        let index = match channels.iter().position(|(i, _, _)| *i == item) {
            Some(index) => index,
            None => {
                channels.push((item, [None; 3], true));
                channels.len() - 1
            }
        };
        let (_, members, single) = &mut channels[index];
        *single &= members[channel.index()].is_none();
        members[channel.index()] = Some(row);
    }
    channels
        .into_iter()
        .filter_map(|(item, members, single)| match members {
            [Some(r), Some(g), Some(b)] if single => {
                super::material::rgb_members(inst, def, ColourGroup::Light, [r, g, b]).map(|m| (item, m))
            }
            _ => None,
        })
        .collect()
}

/// Forces whose targets hold no physics object.
fn idle_forces(def: &EffectGraphDef, items: &[SceneItem]) -> Vec<manifold_core::NodeId> {
    items
        .iter()
        .filter_map(|item| match &item.id {
            SceneItemId::Force(id) => def.scene_modifiers.iter().find(|instance| &instance.id == id),
            _ => None,
        })
        .filter(|instance| {
            let eligible = manifold_node_engine::load::expand::force_objects_for_authoring(def, &instance.scene)
                .unwrap_or_default();
            match &instance.targets {
                SceneTargetSelection::AllObjects => eligible.is_empty(),
                SceneTargetSelection::Explicit { objects } => !eligible.iter().any(|object| objects.contains(object)),
            }
        })
        .map(|instance| instance.id.clone())
        .collect()
}

fn modifier_bound(def: &EffectGraphDef, id: &str) -> bool {
    def.preset_metadata.as_ref().is_some_and(|meta| {
        meta.bindings
            .iter()
            .find(|binding| binding.id == id)
            .is_some_and(|binding| matches!(binding.target, BindingTarget::SceneModifier { .. }))
    })
}

fn user_added(param: &Param, def: &EffectGraphDef, id: &str) -> bool {
    matches!(param.origin, ParamOrigin::UserAdded)
        || def
            .preset_metadata
            .as_ref()
            .is_some_and(|meta| meta.bindings.iter().any(|binding| binding.id == id && binding.user_added))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::cards::{SurfaceVisibility, gen_params_to_surface};
    use manifold_core::PresetTypeId;
    use manifold_core::layer::Layer;
    use manifold_core::project::Project;

    fn project(preset: &str) -> Project {
        let mut project = Project::default();
        project.timeline.layers.push(Layer::new_generator(
            preset.into(),
            PresetTypeId::from_string(preset.to_string()),
            0,
        ));
        project.reconcile_param_manifests();
        project
    }

    fn surface(project: &Project, visibility: SurfaceVisibility) -> Vec<ParamRow> {
        let layer = &project.timeline.layers[0];
        gen_params_to_surface(
            layer.gen_params().expect("generator layer"),
            layer.layer_id.as_str(),
            None,
            &[],
            visibility,
            (manifold_core::Bpm(120.0), 60.0),
        )
        .rows
    }

    fn card(preset: &str) -> Vec<ParamRow> {
        surface(&project(preset), SurfaceVisibility::CuratedCard)
    }

    fn catalog_def(preset: &str) -> EffectGraphDef {
        manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::from_string(preset.to_string()))
            .expect("bundled scene preset")
            .as_ref()
            .clone()
    }

    /// Section titles in card order, each once.
    fn sections(rows: &[ParamRow]) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for row in rows {
            let section = row.spec.section.as_deref().unwrap_or("");
            if out.last() != Some(&section) {
                assert!(!out.contains(&section), "section {section:?} is split: {out:?}");
                out.push(section);
            }
        }
        out
    }

    fn names_in<'a>(rows: &'a [ParamRow], section: &str) -> Vec<&'a str> {
        rows.iter()
            .filter(|row| row.spec.section.as_deref() == Some(section))
            .map(|row| row.spec.name.as_str())
            .collect()
    }

    fn scene_of(project: &Project, id: &str) -> Option<manifold_core::scene_surface::SceneParamDesc> {
        project.timeline.layers[0].gen_params()?.params.get(id)?.spec.scene
    }

    #[test]
    fn orbit_card_order() {
        let rows = card("PhysicsSolids");
        assert_eq!(sections(&rows)[0], "Camera", "camera comes first");
        assert_eq!(
            names_in(&rows, "Camera"),
            ["Orbit", "Tilt", "Distance", "Target Height", "Field of View", "Focus Distance"],
        );
        let focus = rows.iter().find(|row| row.spec.name == "Focus Distance").unwrap();
        assert_eq!(
            focus.spec.inactive_reason.as_deref(),
            Some(Feature::DepthOfField.off_reason()),
            "focus dims while depth of field is off",
        );
    }

    #[test]
    fn free_camera_card_order() {
        let rows = card("Ocean");
        assert_eq!(sections(&rows)[0], "Camera");
        assert_eq!(
            names_in(&rows, "Camera"),
            ["Pan", "Tilt", "Position X", "Position Y", "Position Z", "Field of View", "Focus Distance"],
        );
        assert!(!rows.iter().any(|row| matches!(row.spec.name.as_str(), "Orbit" | "Distance")));
    }

    #[test]
    fn physics_card_bands() {
        let rows = card("PhysicsSolids");
        assert_eq!(sections(&rows), ["Camera", "Physics", "Lighting", "Look"]);
        assert_eq!(names_in(&rows, "Physics"), ["Count", "Reset"]);
        assert_eq!(names_in(&rows, "Lighting"), ["Exposure", "Environment", "Sun", "Fill Light"]);
        assert_eq!(names_in(&rows, "Look"), ["Depth of Field", "Motion Blur"], "details fold while off");
    }

    #[test]
    fn water_card_bands() {
        let rows = card("WaterDamBreakGpuFlip");
        assert_eq!(
            sections(&rows),
            ["Camera", "Water", "Obstacle", "Water — Dam Break (GPU FLIP)", "Lighting", "Look"],
        );
        assert_eq!(names_in(&rows, "Water"), ["Physics View", "Speed", "Reset"]);
        for (id, _) in manifold_nodes_water::presets::gpu_flip::MESHER_CONTROLS {
            assert!(!rows.iter().any(|row| row.id == id), "mesher control {id} is setup, off the card");
        }
        for id in ["sheet_fill_rate", "solve_level", "max_iterations", "particle_capacity"] {
            assert!(!rows.iter().any(|row| row.id.ends_with(id)), "{id} is setup, off the card");
        }
        assert_eq!(names_in(&rows, "Lighting"), ["Exposure", "Environment", "Light", "Rim Light"]);
    }

    #[test]
    fn matter_card_bands() {
        let project = project("WaterDamBreakMatter");
        let rows = surface(&project, SurfaceVisibility::CuratedCard);
        for id in ["cohesion", "stiffness", "liveliness"] {
            let scene = scene_of(&project, id).unwrap_or_else(|| panic!("{id} has a scene description"));
            assert_eq!(scene.role, SurfaceRole::Performance, "{id}");
            assert_eq!(scene.band, Some(CardBand::Scene), "{id}");
        }
        assert_eq!(names_in(&rows, "Water"), ["Speed", "Reset", "Cohesion", "Stiffness", "FLIP Ratio"]);
        for (id, _) in manifold_nodes_water::presets::gpu_flip::MESHER_CONTROLS {
            assert!(!rows.iter().any(|row| row.id == id), "mesher control {id} is setup, off the card");
        }
    }

    #[test]
    fn three_lights_three_named_rows() {
        use manifold_editing::command::Command;
        let mut project = project("PhysicsSolids");
        let def = catalog_def("PhysicsSolids");
        let vm = SceneVm::from_def(&def).expect("scene");
        let target = manifold_core::GraphTarget::Generator(project.timeline.layers[0].layer_id.clone());
        let light_type = vm
            .lights
            .iter()
            .find_map(|light| match light {
                manifold_nodes_scene::node_graph::scene_vm::SceneLightVm::Known(row) => {
                    def.nodes.iter().find(|node| node.node_id == row.node).map(|node| node.type_id.clone())
                }
                _ => None,
            })
            .expect("a known light");
        let mut add = manifold_editing::commands::graph::AddSceneLightCommand::new(
            target,
            Vec::new(),
            vm.scene_root_node_id,
            vm.lights.len() as u32,
            (0.0, 0.0),
            manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type(&light_type),
            def,
        );
        add.execute(&mut project);
        project.reconcile_param_manifests();
        let def = project.timeline.layers[0].gen_params().unwrap().graph.clone().expect("the edit forks the graph");
        let vm = SceneVm::from_def(&def).expect("scene");
        let names: Vec<&str> = vm
            .lights
            .iter()
            .filter_map(|light| match light {
                manifold_nodes_scene::node_graph::scene_vm::SceneLightVm::Known(row) => Some(row.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names.len(), 3);

        let rows = surface(&project, SurfaceVisibility::CuratedCard);
        let lighting = names_in(&rows, "Lighting");
        assert_eq!(lighting[..2], ["Exposure", "Environment"]);
        assert_eq!(lighting[2..], names[..], "one intensity row per light, named by the light");
    }

    #[test]
    fn absent_dof_builds_no_row() {
        let mut project = project("PhysicsSolids");
        let mut def = catalog_def("PhysicsSolids");
        // The depth of field rows: its switch and the details that fold with it.
        let dof_row = |scene: Option<manifold_core::scene_surface::SceneParamDesc>| {
            scene.is_some_and(|scene| {
                scene.switch == Some(Feature::DepthOfField) || scene.gate == Some(Gate::Fold(Feature::DepthOfField))
            })
        };
        // Delete the top-level node holding the switch; its stamped params stay
        // in the manifest, as after a graph-editor delete.
        let dof_nodes: Vec<manifold_core::NodeId> = {
            let inst = project.timeline.layers[0].gen_params().unwrap();
            let meta = def.preset_metadata.as_ref().unwrap();
            inst.params
                .iter()
                .filter(|param| param.spec.scene.is_some_and(|scene| scene.switch == Some(Feature::DepthOfField)))
                .filter_map(|param| meta.bindings.iter().find(|binding| binding.id == param.id()))
                .filter_map(|binding| match &binding.target {
                    manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. } => Some(node_id.clone()),
                    _ => None,
                })
                .collect()
        };
        let holds = |node: &manifold_core::effect_graph_def::EffectGraphNode| {
            dof_nodes.contains(&node.node_id)
                || node.group.as_ref().is_some_and(|group| group.nodes.iter().any(|n| dof_nodes.contains(&n.node_id)))
        };
        let removed: Vec<u32> = def.nodes.iter().filter(|node| holds(node)).map(|node| node.id).collect();
        assert!(!removed.is_empty(), "the native scene carries a depth of field chain");
        def.nodes.retain(|node| !removed.contains(&node.id));
        def.wires.retain(|wire| !removed.contains(&wire.from_node) && !removed.contains(&wire.to_node));
        project.timeline.layers[0].gen_params_mut().unwrap().graph = Some(def);

        let rows = surface(&project, SurfaceVisibility::CuratedCard);
        assert!(!rows.is_empty());
        for row in &rows {
            assert!(!dof_row(scene_of(&project, &row.id)), "{} builds for a feature the scene lacks", row.id);
        }
        let focus = rows.iter().find(|row| row.spec.name == "Focus Distance").expect("the lens stays");
        assert_eq!(focus.spec.inactive_reason.as_deref(), Some(Feature::DepthOfField.off_reason()));
    }

    /// D12 (row sets follow authored values): a driver lifting the effective
    /// value never builds the detail rows; the base value does.
    #[test]
    fn look_rows_follow_base_not_effective() {
        let mut project = project("PhysicsSolids");
        let switch = project.timeline.layers[0]
            .gen_params()
            .unwrap()
            .params
            .iter()
            .find(|param| param.spec.scene.is_some_and(|scene| scene.switch == Some(Feature::DepthOfField)))
            .map(|param| param.id().to_string())
            .expect("a depth of field switch");
        let folded = |project: &Project, rows: &[ParamRow]| {
            rows.iter()
                .filter(|row| {
                    scene_of(project, &row.id)
                        .is_some_and(|scene| scene.gate == Some(Gate::Fold(Feature::DepthOfField)))
                })
                .count()
        };
        let set = |project: &mut Project, base: f32, effective: f32| {
            let param = project.timeline.layers[0].gen_params_mut().unwrap().params.get_mut(&switch).unwrap();
            param.base = base;
            param.value = effective;
        };

        set(&mut project, 0.0, 1.0);
        let rows = surface(&project, SurfaceVisibility::CuratedCard);
        assert_eq!(folded(&project, &rows), 0, "the effective value built detail rows");

        set(&mut project, 1.0, 0.0);
        let rows = surface(&project, SurfaceVisibility::CuratedCard);
        assert_eq!(folded(&project, &rows), 2, "F-Stop and Max Blur build while the base is on");
        assert!(rows.iter().any(|row| row.spec.name == "F-Stop"));
    }

    /// INV-S8: a row reads the same on the card and in the Scene tab, except
    /// item- and group-named rows, which read the item or group title.
    #[test]
    fn card_and_panel_labels_match() {
        for preset in ["PhysicsSolids", "Ocean", "WaterDamBreakGpuFlip", "WaterDamBreakMatter", "PhysicsBoxes"] {
            let project = project(preset);
            let def = catalog_def(preset);
            let vm = SceneVm::from_def(&def).expect("scene");
            let items = scene_items(&def, &vm);
            let meta = def.preset_metadata.as_ref().expect("metadata");
            let panel = surface(&project, SurfaceVisibility::All);
            for row in surface(&project, SurfaceVisibility::CuratedCard) {
                let in_panel = panel.iter().find(|p| p.id == row.id).expect("every card row is a panel row");
                let param = project.timeline.layers[0].gen_params().unwrap().params.get(&row.id).unwrap();
                let expected = match param.spec.scene.map(|scene| scene.card_label) {
                    _ if param.choices.rename.is_some() => in_panel.spec.name.clone(),
                    Some(CardLabel::ItemName) => {
                        items[owner_of(&items, meta, &row.id).expect("an owned row")].name.clone()
                    }
                    Some(CardLabel::GroupName) => param.spec.scene.unwrap().group.title().to_string(),
                    _ => in_panel.spec.name.clone(),
                };
                assert_eq!(row.spec.name, expected, "{preset}: {}", row.id);
            }
        }
    }

    fn attach(row: &mut ParamRow, kind: &str) {
        match kind {
            "driver" => row.modulation.driver_active = true,
            "envelope" => row.modulation.envelope_active = true,
            "automation" => row.modulation.automation_active = true,
            "audio" => row.audio.active = true,
            "ableton" => {
                row.mapping.ableton_display = Some(manifold_ui::panels::param_slider_shared::AbletonMappingDisplay {
                    macro_name: "Macro 1".into(),
                    track_name: "Track".into(),
                    device_name: "Rack".into(),
                    status: Default::default(),
                    inverted: false,
                })
            }
            other => unreachable!("{other}"),
        }
    }

    /// INV-S5 (D6): card membership is the stored pin, never the live
    /// attachment. An attach pins a Setup or Hidden row Shown
    /// (`PresetInstance::keep_on_card`); with the pin it stays after every
    /// modulator is gone, and an attachment without a pin builds no row.
    #[test]
    fn attached_rows_stay_on_card() {
        let project = project("WaterDamBreakGpuFlip");
        let def = catalog_def("WaterDamBreakGpuFlip");
        let all = surface(&project, SurfaceVisibility::All);
        let setup: Vec<String> = all
            .iter()
            .filter(|row| scene_of(&project, &row.id).is_some_and(|scene| scene.role == SurfaceRole::Setup))
            .map(|row| row.id.to_string())
            .take(2)
            .collect();
        assert_eq!(setup.len(), 2, "two setup rows");
        // A hidden param that an older table stamped keeps its id and row
        // input; the bundled presets stamp none, so mark one here.
        let mut inst = project.timeline.layers[0].gen_params().unwrap().clone();
        let hidden = &setup[1];
        let param = inst.params.get_mut(hidden).unwrap();
        let mut scene = param.spec.scene.unwrap();
        scene.role = SurfaceRole::Hidden;
        param.spec.scene = Some(scene);
        let on_card = |inst: &PresetInstance, rows: &[ParamRow], id: &str| {
            let mut rows = rows.to_vec();
            assert!(scene_card_rows(&mut rows, inst, &def));
            rows.iter().any(|row| row.id == id)
        };
        for id in [&setup[0], hidden] {
            assert!(!on_card(&inst, &all, id), "{id} is off the card unattached");
            for kind in ["driver", "envelope", "automation", "audio", "ableton"] {
                let mut rows = all.clone();
                attach(rows.iter_mut().find(|row| row.id == id.as_str()).unwrap(), kind);
                assert!(!on_card(&inst, &rows, id), "{id} with {kind} but no pin builds no row");
            }
            let mut pinned = inst.clone();
            assert_eq!(pinned.keep_on_card(id), Some(None), "the attach pins {id}");
            assert_eq!(pinned.keep_on_card(id), None, "a second attach writes nothing");
            assert!(on_card(&pinned, &all, id), "{id} stays with its modulator gone");
        }
        // Remove from Card gives way to a later attach; a Performance row
        // is on the card by its role and takes no pin.
        let mut removed = inst.clone();
        removed.params.get_mut(&setup[0]).unwrap().choices.pin = Some(CardPin::Removed);
        assert_eq!(removed.keep_on_card(&setup[0]), Some(Some(CardPin::Removed)));
        let performance = all
            .iter()
            .find(|row| {
                scene_of(&project, &row.id)
                    .is_some_and(|scene| scene.role == SurfaceRole::Performance && !matches!(scene.gate, Some(Gate::Fold(_))))
            })
            .expect("a performance row");
        assert_eq!(inst.clone().keep_on_card(&performance.id), None);
    }

    /// A pinned row whose feature is off stays on the card, dimmed with the
    /// feature's reason, never looking live (D6, D12).
    #[test]
    fn pinned_folded_row_shows_dimmed() {
        let mut project = project("PhysicsSolids");
        let host = project.timeline.layers[0].gen_params_mut().unwrap();
        let fold = |param: &Param| param.spec.scene.is_some_and(|scene| scene.gate == Some(Gate::Fold(Feature::DepthOfField)));
        let switch_on = host
            .params
            .iter()
            .any(|param| param.spec.scene.is_some_and(|scene| scene.switch == Some(Feature::DepthOfField)) && param.base > 0.5);
        assert!(!switch_on, "depth of field starts off");
        let id = host.params.iter().find(|param| fold(param)).map(|param| param.id().to_string()).expect("a folded row");
        assert!(!surface(&project, SurfaceVisibility::CuratedCard).iter().any(|row| row.id == id.as_str()));
        project.timeline.layers[0].gen_params_mut().unwrap().params.get_mut(&id).unwrap().choices.pin = Some(CardPin::Shown);
        let rows = surface(&project, SurfaceVisibility::CuratedCard);
        let row = rows.iter().find(|row| row.id == id.as_str()).expect("the pinned row builds");
        assert_eq!(row.spec.inactive_reason.as_deref(), Some(Feature::DepthOfField.off_reason()));
    }

    fn folds(project: &Project) -> Vec<String> {
        let inst = project.timeline.layers[0].gen_params().unwrap();
        let def = inst.graph.clone().unwrap_or_else(|| catalog_def(inst.effect_type().as_str()));
        folded_sections(inst, &def, &surface(project, SurfaceVisibility::CuratedCard)).expect("a scene card")
    }

    /// P7 round trip: a Setup row shown on the card, Orbit removed from it
    /// and Lighting folded all hold across save and reload, and the driver on
    /// the pinned row still drives it.
    #[test]
    fn pins_and_folds_survive_save_and_reload() {
        use manifold_editing::command::Command;
        let mut project = project("PhysicsSolids");
        let target = manifold_core::GraphTarget::Generator(project.timeline.layers[0].layer_id.clone());
        let card_ids = |project: &Project| -> Vec<String> {
            surface(project, SurfaceVisibility::CuratedCard).iter().map(|row| row.id.to_string()).collect()
        };
        let before = card_ids(&project);
        let pinned = surface(&project, SurfaceVisibility::All)
            .into_iter()
            .find(|row| row.value.exposed && !row.spec.is_toggle && !before.contains(&row.id.to_string()))
            .expect("a Setup row off the card")
            .id
            .to_string();
        let orbit = surface(&project, SurfaceVisibility::CuratedCard)
            .into_iter()
            .find(|row| row.spec.name == "Orbit")
            .expect("Orbit on the card")
            .id
            .to_string();
        manifold_editing::commands::graph::SetCardPinCommand::new(target.clone(), pinned.clone(), Some(CardPin::Shown))
            .execute(&mut project);
        manifold_editing::commands::graph::SetCardPinCommand::new(target, orbit.clone(), Some(CardPin::Removed))
            .execute(&mut project);
        let fx = project.timeline.layers[0].gen_params_mut().unwrap();
        fx.folded_sections = vec!["Lighting".into()];
        fx.drivers.get_or_insert_with(Vec::new).push(manifold_core::effects::ParameterDriver::new(
            pinned.clone(),
            manifold_core::types::BeatDivision::Quarter,
            manifold_core::types::DriverWaveform::Sine,
        ));

        let json = serde_json::to_string(&project).expect("save");
        let reloaded = manifold_io::loader::load_project_from_json(&json).expect("reload");
        for project in [&project, &reloaded] {
            let rows = surface(project, SurfaceVisibility::CuratedCard);
            let pinned_row = rows.iter().find(|row| row.id == pinned.as_str()).expect("the pinned row is on the card");
            assert!(pinned_row.modulation.driver_active, "the driver still drives the pinned row");
            assert!(!rows.iter().any(|row| row.id == orbit.as_str()), "Orbit stays off the card");
            assert_eq!(folds(project), ["Lighting"]);
        }
    }

    /// D11: Lighting starts folded past eight light rows; the user's unfold
    /// then holds.
    #[test]
    fn lighting_folds_past_eight_lights_until_unfolded() {
        use manifold_editing::command::Command;
        let mut project = project("PhysicsSolids");
        let target = manifold_core::GraphTarget::Generator(project.timeline.layers[0].layer_id.clone());
        let lights = |project: &Project| {
            names_in(&surface(project, SurfaceVisibility::CuratedCard), "Lighting").len() - 2
        };
        while lights(&project) <= LIGHT_ROWS_BEFORE_FOLD {
            assert!(!folds(&project).contains(&"Lighting".to_string()), "{} lights fold Lighting", lights(&project));
            let def = project.timeline.layers[0]
                .gen_params()
                .unwrap()
                .graph
                .clone()
                .unwrap_or_else(|| catalog_def("PhysicsSolids"));
            let vm = SceneVm::from_def(&def).expect("scene");
            let light_type = vm
                .lights
                .iter()
                .find_map(|light| match light {
                    manifold_nodes_scene::node_graph::scene_vm::SceneLightVm::Known(row) => {
                        def.nodes.iter().find(|node| node.node_id == row.node).map(|node| node.type_id.clone())
                    }
                    _ => None,
                })
                .expect("a known light");
            manifold_editing::commands::graph::AddSceneLightCommand::new(
                target.clone(),
                Vec::new(),
                vm.scene_root_node_id,
                vm.lights.len() as u32,
                (0.0, 0.0),
                manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type(&light_type),
                def,
            )
            .execute(&mut project);
            project.reconcile_param_manifests();
        }
        assert_eq!(lights(&project), LIGHT_ROWS_BEFORE_FOLD + 1);
        assert_eq!(folds(&project), ["Lighting"], "nine lights fold Lighting by default");
        project.timeline.layers[0].gen_params_mut().unwrap().unfolded_sections = vec!["Lighting".into()];
        assert!(folds(&project).is_empty(), "the user's unfold beats the default");
    }

    /// INV-S4: the card and the Scene tab give every owned row the same
    /// item, over the headless scene fixtures.
    #[cfg(feature = "ui-snapshot")]
    #[test]
    fn card_and_panel_agree_on_owners() {
        let mut cache = crate::ui_snapshot::fixtures::FixtureCache::default();
        for scene in ["gltfscene", "physicsscene", "waterdambreak", "waterdambreakmatter", "oceanscene"] {
            let data = crate::ui_snapshot::fixtures::build_cached(scene, &mut cache).expect("fixture");
            let index = data.active.expect("a selected scene layer");
            let layer = &data.project.timeline.layers[index];
            let inst = layer.gen_params().expect("generator");
            let def = match &inst.graph {
                Some(def) => def.clone(),
                None => catalog_def(inst.effect_type().as_str()),
            };
            let vm = SceneVm::from_def(&def).expect("scene");
            let items = scene_items(&def, &vm);
            let meta = def.preset_metadata.as_ref().expect("metadata");
            let panel: Vec<Vec<String>> = items
                .iter()
                .map(|item| super::super::scene::parameter_ids_for_nodes(Some(&def), &item.owned_nodes))
                .collect();
            let rows = gen_params_to_surface(
                inst,
                layer.layer_id.as_str(),
                None,
                &[],
                SurfaceVisibility::CuratedCard,
                (manifold_core::Bpm(120.0), 60.0),
            )
            .rows;
            let mut checked = 0;
            for row in &rows {
                let Some(owner) = owner_of(&items, meta, &row.id) else { continue };
                if items[owner].owned_nodes.is_empty() {
                    continue;
                }
                let holders: Vec<&str> = panel
                    .iter()
                    .zip(&items)
                    .filter(|(ids, _)| ids.iter().any(|id| id == row.id.as_ref()))
                    .map(|(_, item)| item.name.as_str())
                    .collect();
                assert_eq!(holders, [items[owner].name.as_str()], "{scene}: {}", row.id);
                checked += 1;
            }
            assert!(checked > 0, "{scene}: no owned card rows");
        }
    }

    /// A non-force modifier's controls (Render Mode, Surface Waves) live on
    /// the modifier's own card; the scene card shows them only when the user
    /// attaches, pins or adds them.
    #[test]
    fn modifier_controls_without_scene_fields_stay_off_the_card() {
        let param = Param::bundled(manifold_core::effect_graph_def::ParamSpecDef {
            id: "mode".into(),
            card_visible: true,
            ..Default::default()
        });
        assert!(param.spec.scene.is_none());
        assert!(Placement::of(&param, None, false, true).is_none());
        assert!(Placement::of(&param, None, true, true).is_some(), "user-added rows stay");
        let mut pinned = param.clone();
        pinned.choices.pin = Some(CardPin::Shown);
        assert!(Placement::of(&pinned, None, false, true).is_some(), "Show on Card and attaches (D6) win");
        assert!(Placement::of(&param, None, false, false).is_some(), "authored controls keep card visibility");
    }
}
