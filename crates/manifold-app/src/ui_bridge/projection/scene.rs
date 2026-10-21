//! Scene tab projection: the Scene tab's per-frame row value sync
//! and the exposure-section doc-id resolver. Moved from state_sync.rs (P-P,
//! UI_FUNNEL_DECOMPOSITION_DESIGN.md).

use crate::ui_root::UIRoot;
use manifold_core::project::Project;
use manifold_core::SceneNodeRef;

/// Every liquid domain behind a scene object, named after its water.
pub(crate) fn fluid_domains(
    scene: &manifold_nodes_scene::node_graph::scene_vm::SceneVm,
) -> Vec<manifold_ui::panels::scene_setup_panel::FluidDomainOption> {
    let mut result: Vec<manifold_ui::panels::scene_setup_panel::FluidDomainOption> = Vec::new();
    for object in &scene.objects {
        let manifold_nodes_scene::node_graph::scene_vm::SceneObjectVm::Known(row) = object else { continue; };
        let Some(domain) = &row.liquid_domain else { continue; };
        let rows = std::iter::once(row.address.clone()).chain(row.parent.clone());
        match result.iter_mut().find(|option| option.node == domain.node) {
            Some(option) => option.rows.extend(rows),
            None => result.push(manifold_ui::panels::scene_setup_panel::FluidDomainOption {
                node: domain.node.clone(), name: row.name.clone(), rows: rows.collect(),
            }),
        }
    }
    result
}

pub(crate) fn fluid_role_rows(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    group_id: Option<u32>,
    domains: &[manifold_ui::panels::scene_setup_panel::FluidDomainOption],
) -> Result<Vec<manifold_ui::panels::scene_setup_panel::FluidRoleRow>, String> {
    let Some(group_id) = group_id else { return Ok(Vec::new()); };
    manifold_editing::commands::graph::scene_fluid_role_assignments(def, group_id).map(|roles| {
        roles.into_iter().map(|role| {
            let target_label = match role.domains.as_slice() {
                [] => "Choose Fluid".into(),
                [target] => domains.iter().find(|domain| domain.node == target.node)
                    .map(|domain| format!("Target: {}", domain.name))
                    .unwrap_or_else(|| "Fluid outside this scene".into()),
                targets => format!("Targets: {} fluids", targets.len()),
            };
            manifold_ui::panels::scene_setup_panel::FluidRoleRow {
                source: role.source, name: role.name, target_label,
            }
        }).collect()
    })
}

/// The nodes scene item `id` owns (SCENE_SURFACES_DESIGN.md D3 (one scene
/// item list)); empty when the scene has no such item.
pub(crate) fn owned_nodes<'a>(
    items: &'a [manifold_nodes_scene::node_graph::scene_items::SceneItem],
    id: &manifold_nodes_scene::node_graph::scene_items::SceneItemId,
) -> &'a [manifold_core::NodeId] {
    items.iter().find(|item| &item.id == id).map_or(&[], |item| item.owned_nodes.as_slice())
}

/// The nodes of the World and every physics world, for tests that read
/// their stored section strings.
#[cfg(test)]
pub(crate) fn world_and_physics_nodes(
    items: &[manifold_nodes_scene::node_graph::scene_items::SceneItem],
) -> Vec<manifold_core::NodeId> {
    use manifold_core::scene_surface::SceneItemKind;
    items
        .iter()
        .filter(|item| matches!(item.kind, SceneItemKind::World | SceneItemKind::Physics))
        .flat_map(|item| item.owned_nodes.iter().cloned())
        .collect()
}

/// The Scene tab tree's items (SCENE_SURFACES D8) from the scene items and
/// the panel rows already projected from the same `SceneVm`. Lights and
/// objects pair with their Known rows by the node the item names. `object_ids` are each
/// Known object's filtered control ids; every other item owns its nodes' controls.
///
/// One Lighting item follows Camera: it takes the Camera's and World's
/// lighting controls (a Lighting-band row such as exposure, and every
/// Environment-group row), and the Scene tab shows every light inside it.
/// The per-light items stay, since lights keep their own names and rows.
pub(crate) fn scene_item_vms(
    def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    items: &[manifold_nodes_scene::node_graph::scene_items::SceneItem],
    objects: &[(SceneNodeRef, Option<SceneNodeRef>)],
    lights: &[SceneNodeRef],
    object_ids: Vec<Vec<String>>,
    scene_of: &dyn Fn(&str) -> Option<manifold_core::scene_surface::SceneParamDesc>,
) -> Vec<manifold_ui::panels::scene_setup_panel::SceneItemVm> {
    use manifold_core::scene_surface::{CardBand, ControlGroup, SceneItemKind};
    use manifold_nodes_scene::node_graph::scene_items::SceneItemId;
    use manifold_ui::panels::scene_setup_panel::{SceneItemVm, SceneSelection};
    let lighting_row = |id: &String| {
        scene_of(id).is_some_and(|scene| scene.band == Some(CardBand::Lighting) || scene.group == ControlGroup::Environment)
    };
    // An item finds its row by the node it names; the order of the two lists
    // is never relied on. `object_ids` runs parallel to the Known object rows.
    let known_objects = objects;
    let known_lights = lights;
    let mut object_ids: Vec<Option<Vec<String>>> = object_ids.into_iter().map(Some).collect();
    let mut lighting_ids = Vec::new();
    let mut out: Vec<SceneItemVm> = items
        .iter()
        .filter_map(|item| {
            let mut owned = || {
                let mut ids = parameter_ids_for_nodes(def, &item.owned_nodes);
                if matches!(item.id, SceneItemId::Camera | SceneItemId::World) {
                    lighting_ids.extend(ids.extract_if(.., |id| lighting_row(id)));
                }
                ids
            };
            let (selection, parent, parameter_ids) = match &item.id {
                SceneItemId::Camera => (SceneSelection::Camera, None, owned()),
                SceneItemId::World => (SceneSelection::World, None, owned()),
                SceneItemId::Light(id) => {
                    let light = known_lights.iter().find(|light| &light.node == id)?;
                    (SceneSelection::Light(light.clone()), None, owned())
                }
                SceneItemId::Object(id) => {
                    let at = known_objects.iter().position(|(row, _)| &row.node == id)?;
                    let (row, parent) = &known_objects[at];
                    (SceneSelection::Object(row.clone()), parent.clone(), object_ids.get_mut(at)?.take()?)
                }
                SceneItemId::Force(id) => (SceneSelection::Force(id.clone()), None, Vec::new()),
                SceneItemId::Physics(id) => (SceneSelection::Physics(id.clone()), None, owned()),
                SceneItemId::Source { node, water } => (SceneSelection::Source(node.clone()), Some(water.clone()), owned()),
            };
            Some(SceneItemVm { kind: item.kind, selection, name: item.name.clone(), parent, parameter_ids })
        })
        .collect();
    let at = out.iter().position(|item| item.selection == SceneSelection::Camera).map_or(0, |camera| camera + 1);
    out.insert(at, SceneItemVm {
        kind: SceneItemKind::Light,
        selection: SceneSelection::Lighting,
        name: "Lighting".into(),
        parent: None,
        parameter_ids: lighting_ids,
    });
    out
}

/// The generator string param that loads the HDRI node `hdri_doc_id`'s file:
/// the one whose string binding targets that node's `path`. Its index is
/// the layer card's string row index, so the Scene tab opens the same edit.
pub(crate) fn hdri_string_param(
    def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    preset: &manifold_core::PresetTypeId,
    hdri_doc_id: u32,
) -> Option<usize> {
    use manifold_core::effect_graph_def::BindingTarget;
    let def = def?;
    let binding = def.preset_metadata.as_ref()?.string_bindings.iter().find(|binding| match &binding.target {
        BindingTarget::Node { node_id, param } => {
            param == "path"
                && super::super::dispatch::params::find_effect_string_node(&def.nodes, node_id, &mut Vec::new())
                    .is_some_and(|(doc_id, _)| doc_id == hdri_doc_id)
        }
        _ => false,
    })?;
    manifold_core::preset_definition_registry::try_get(preset)?
        .string_param_defs
        .iter()
        .position(|sp| sp.key == binding.id)
}

/// Family eyes and look rows use the existing manifest, with one visible
/// control per row. A child does not acquire scene-object rendering controls.
pub(crate) fn filter_family_parameter_ids(
    def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    row: &manifold_nodes_scene::node_graph::scene_vm::SceneObjectKnownRow,
    ids: &mut Vec<String>,
) {
    let Some(metadata) = def.and_then(|def| def.preset_metadata.as_ref()) else { return; };
    use manifold_core::scene_surface::SceneObjectRowKind;
    let look = row.kind == SceneObjectRowKind::LookChild;
    let water = row.kind == SceneObjectRowKind::PhysicsParent && row.liquid_domain.is_some();
    if !look && !water { return; }
    ids.retain(|id| {
        let Some(binding) = metadata.bindings.iter().find(|binding| &binding.id == id) else { return false; };
        match &binding.target {
            manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } if node_id == &row.object => {
                if look { param == "visible" } else { param != "visible" }
            }
            _ => true,
        }
    });
}

/// Per-frame VALUE sync for the Scene tab's rows — the scene-row
/// sibling of [`sync_card_values`]: push each built row's CURRENT value from
/// `project` (the layer's generator graph def, instance override or bundled
/// default) onto the already-built panel, so rows track OSC / command /
/// other-window writes between structural syncs instead of freezing. Driven
/// (wire-fed) rows update through the value-label handle the driven branch
/// now keeps; non-driven rows update their card slider. Same drag safety as
/// `sync_card_values`: the actively-dragged field is restored into
/// `local_project` upstream of every call, so this writes the user's own
/// value straight back. No-op while the panel is closed or not Live.
pub fn sync_scene_row_values(ui: &mut UIRoot, project: &Project) {
    if !ui.scene_setup_panel.is_open() {
        return;
    }
    let Some(layer_id) = ui.scene_setup_panel.live_layer_id() else {
        return;
    };
    let Some((_, layer)) = project.timeline.find_layer_by_id(layer_id.as_str()) else {
        return;
    };
    let gen_inst = layer.gen_params();

    // The unified properties card's per-frame value push — real exposed
    // params, resolved the SAME id-keyed way `sync_card_values` resolves the
    // main generator inspector card's values (`ui_translate::with_param_slots`),
    // just against the SCENE PANEL's own bound layer (`live_layer_id`) rather
    // than the app's `active_layer` (a scene row always lives on the layer its
    // panel is docked to, which can differ from the app's active layer —
    // BUG-292). The panel JOINS by id (BUG-313): a manifest param the current
    // outliner selection isn't showing simply misses the join and is ignored.
    if let Some(gp) = gen_inst {
        crate::ui_translate::with_param_slots(&gp.params, |slots| {
            ui.scene_setup_panel
                .sync_properties_values(&mut ui.tree, slots)
        });
    }
}

/// The outliner eyes' in-place push (D22, planned as `SceneEyes`): each eye
/// reads its write address the way the scene projection does, the binding's
/// instance slot for a bound row, else the def's node param (default on).
/// Edit-rate only: the binding lookup allocates.
pub fn sync_scene_outliner_eyes(ui: &mut UIRoot, project: &Project) {
    if !ui.scene_setup_panel.is_open() {
        return;
    }
    let Some(layer_id) = ui.scene_setup_panel.live_layer_id().cloned() else {
        return;
    };
    let Some((_, layer)) = project.timeline.find_layer_by_id(layer_id.as_str()) else {
        return;
    };
    let bundled;
    let def = match layer.generator_graph() {
        Some(def) => def,
        None => {
            bundled = manifold_nodes::bundled_presets::bundled_preset_def(layer.generator_type());
            let Some(def) = bundled.as_deref() else { return };
            def
        }
    };
    let inst = layer.gen_params();
    let mut value_of = |addr: &manifold_ui::panels::scene_setup_panel::RowAddr| {
        use crate::ui_bridge::project::{find_node_by_scope, is_water_object, scene_param_value};
        let (scope, node, param) = (addr.scope_path.as_slice(), addr.node_doc_id, addr.param_id.as_str());
        let bound = inst.is_some_and(|inst| {
            crate::ui_bridge::project::scene_binding_id(def, scope, node, param).is_some_and(|id| inst.params.contains(id.as_str()))
        });
        if !bound && find_node_by_scope(def, scope, node).is_none() {
            return None;
        }
        let value = scene_param_value(def, inst, scope, node, param);
        // A water's eye shows it as drawn: hidden by either flag.
        if param == "parent_visible" && is_water_object(def, scope, node) {
            return Some(value.min(scene_param_value(def, inst, scope, node, "visible")));
        }
        Some(value)
    };
    ui.scene_setup_panel.sync_outliner_eyes(&mut ui.tree, &mut value_of);
}

/// The real section strings (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md P2
/// slice 2a) of every exposed control owned by `nodes`, read off the def's
/// exposure metadata: creation-time and load-time stamping name the same
/// node kind's section differently, so only the stored string filters
/// correctly. Dedups, preserves first-seen order.
///
/// Deliberately does NOT filter by `spec.card_visible`: that flag gates the
/// generator's outer card, never the scene panel.
/// The Scene tab partitions by item ownership now; tests still read the
/// stored strings to check what the importers stamp.
#[cfg(test)]
pub(crate) fn sections_for_nodes(
    def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    nodes: &[manifold_core::NodeId],
) -> Vec<String> {
    let mut sections: Vec<String> = Vec::new();
    for spec in owned_specs(def, nodes) {
        if let Some(section) = &spec.section
            && !sections.contains(section)
        {
            sections.push(section.clone());
        }
    }
    sections
}

/// The exact exposed parameter ids owned by `nodes`; shares its predicate
/// with [`sections_for_nodes`].
pub(crate) fn parameter_ids_for_nodes(
    def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    nodes: &[manifold_core::NodeId],
) -> Vec<String> {
    owned_specs(def, nodes).map(|spec| spec.id.clone()).collect()
}

/// A control belongs to the node its PRIMARY binding (the first under its
/// id) targets. A fan-out control (the glTF importer's sun position also
/// driving the environment's sun, BUG-291) keeps one owner however many
/// nodes it drives. Stable node ids are unique across the document; the
/// numeric prefix of a stamped id is a group-local doc id and names no
/// owner.
fn owned_specs<'a>(
    def: Option<&'a manifold_core::effect_graph_def::EffectGraphDef>,
    nodes: &'a [manifold_core::NodeId],
) -> impl Iterator<Item = &'a manifold_core::effect_graph_def::ParamSpecDef> + 'a {
    let meta = def.and_then(|def| def.preset_metadata.as_ref()).filter(|_| !nodes.is_empty());
    meta.into_iter().flat_map(move |meta| {
        meta.params.iter().filter(move |spec| {
            meta.bindings.iter().find(|binding| binding.id == spec.id).is_some_and(|binding| {
                matches!(&binding.target,
                    manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. } if nodes.contains(node_id))
            })
        })
    })
}

/// Remove only the legacy rigid-body geometry controls that are inactive when
/// a body is driven by a mesh source. `parameter_ids` is already owned by the
/// scene row's original projection predicate; this helper only subtracts
/// bindings that target the exact scoped body node and have no effective
/// target elsewhere.
pub(crate) fn filter_inactive_physics_parameter_ids(
    def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    physics: Option<&manifold_nodes_scene::node_graph::scene_vm::PhysicsVm>,
    parameter_ids: &mut Vec<String>,
) {
    use manifold_core::effect_graph_def::BindingTarget;

    let Some(def) = def else { return };
    let Some(physics) = physics else { return };
    let mut nodes = def.nodes.as_slice();
    let mut wires = def.wires.as_slice();
    for group_id in &physics.body_scope_path {
        let Some(group) = nodes
            .iter()
            .find(|node| node.id == *group_id)
            .and_then(|node| node.group.as_deref())
        else {
            return;
        };
        nodes = &group.nodes;
        wires = &group.wires;
    }
    let Some(body) = nodes.iter().find(|node| node.id == physics.body_node_id) else {
        return;
    };
    if !wires.iter().any(|wire| {
        wire.to_node == physics.body_node_id && wire.to_port == "source"
    }) {
        return;
    }
    let Some(metadata) = def.preset_metadata.as_ref() else { return; };
    retain_effective_binding_targets(metadata, parameter_ids, |target| {
        matches!(target,
            BindingTarget::Node { node_id, param }
                if node_id == &body.node_id
                    && matches!(param.as_str(), "shape" | "collider_parts"))
    });
}

/// Remove fallback controls from a fluid role whose geometry comes from one or
/// more authoritative `mesh_N` inputs. The role node is found by stable graph
/// identity while walking the selected object's complete nested group scope.
pub(crate) fn filter_inactive_fluid_role_parameter_ids(
    def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    group_id: Option<u32>,
    parameter_ids: &mut Vec<String>,
) {
    use manifold_core::effect_graph_def::BindingTarget;

    let Some(def) = def else { return; };
    let (nodes, wires) = match group_id {
        Some(id) => {
            let Some(group) = def.nodes.iter().find(|node| node.id == id)
                .and_then(|node| node.group.as_deref()) else { return; };
            (&group.nodes, &group.wires)
        }
        None => (&def.nodes, &def.wires),
    };
    let mut wired_role_nodes = Vec::new();
    collect_wired_fluid_role_nodes(nodes, wires, &mut wired_role_nodes);
    if wired_role_nodes.is_empty() {
        return;
    }
    let Some(metadata) = def.preset_metadata.as_ref() else { return; };
    retain_effective_binding_targets(metadata, parameter_ids, |target| {
        matches!(target,
            BindingTarget::Node { node_id, param }
                if wired_role_nodes.iter().any(|role| role == node_id)
                    && matches!(param.as_str(),
                        "shape" | "radius" | "path" | "mesh_index" |
                        "primitive_index" | "material_index" | "compound_materials"))
    });
}

fn collect_wired_fluid_role_nodes(
    nodes: &[manifold_core::effect_graph_def::EffectGraphNode],
    wires: &[manifold_core::effect_graph_def::EffectGraphWire],
    result: &mut Vec<manifold_core::NodeId>,
) {
    for node in nodes {
        if node.type_id == "node.fluid_role_source"
            && !node.node_id.is_empty()
            && wires.iter().any(|wire| {
                wire.to_node == node.id
                    && wire.to_port.strip_prefix("mesh_")
                        .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
            })
        {
            result.push(node.node_id.clone());
        }
        if let Some(group) = node.group.as_deref() {
            collect_wired_fluid_role_nodes(&group.nodes, &group.wires, result);
        }
    }
}

fn retain_effective_binding_targets(
    metadata: &manifold_core::effect_graph_def::PresetMetadata,
    parameter_ids: &mut Vec<String>,
    mut is_inactive: impl FnMut(&manifold_core::effect_graph_def::BindingTarget) -> bool,
) {
    parameter_ids.retain(|parameter_id| {
        let mut has_inactive_target = false;
        let mut has_effective_target = false;
        for binding in metadata
            .bindings
            .iter()
            .filter(|binding| binding.id.as_str() == parameter_id.as_str())
        {
            if is_inactive(&binding.target) {
                has_inactive_target = true;
            } else {
                has_effective_target = true;
            }
        }
        !has_inactive_target || has_effective_target
    });
}

#[cfg(test)]
mod ownership_tests {
    //! BUG-291 (fan-out leak): reproduces the glTF importer's D7
    //! sun-coherence fan-out that leaked a "Sun" section into World's item,
    //! plus the grouped-preset doc id collisions that ownership by stable
    //! node id removes.
    use super::*;
    use manifold_core::NodeId;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef,
        EffectGraphNode, EffectGraphWire, GroupDef, GroupInterface, InterfacePortDef,
        ParamSpecDef, PresetMetadata,
    };
    use manifold_core::effects::ParamConvert;
    use manifold_nodes_scene::node_graph::scene_vm::PhysicsVm;
    use std::collections::{BTreeMap, BTreeSet};

    /// World = envmap (doc id 1) [+ atmosphere, omitted — not needed to
    /// reproduce the leak]. Sun = its own light node (doc id 7). The sun's
    /// `pos_x` control fans out to `envmap.sun_x` (D7 "sun coherence") under
    /// the SAME `id` as its own `sun.pos_x` binding — exactly the shape that
    /// made the old target-walking implementation attribute the fanned-out
    /// binding to World (whose doc-id set contains the envmap node the
    /// fan-out targets).
    fn azalea_like_fixture() -> EffectGraphDef {
        let meta = PresetMetadata {
            shape: None,
            scene_modifier: None,
            id: PresetTypeId::new("gltf_import_fixture"),
            display_name: "glTF Import Fixture".to_string(),
            category: "Diagnostic".to_string(),
            osc_prefix: "gltf_import_fixture".to_string(),
            legacy_discriminant: None,
            scene_bounds: None,
            available: true,
            is_line_based: false,
            layer_types: None,
            params: vec![
                ParamSpecDef {
                    id: "1_intensity".to_string(),
                    name: "Intensity".to_string(),
                    section: Some("Environment".to_string()),
                    ..Default::default()
                },
                ParamSpecDef {
                    id: "7_pos_x".to_string(),
                    name: "Position X".to_string(),
                    section: Some("Sun".to_string()),
                    ..Default::default()
                },
            ],
            bindings: vec![
                // envmap's own intensity binding.
                BindingDef {
                    id: "1_intensity".to_string(),
                    label: String::new(),
                    default_value: 1.0,
                    target: BindingTarget::Node {
                        node_id: NodeId::new("envmap"),
                        param: "intensity".to_string(),
                    },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                    default_mirrors_node_param: false,
                },
                // The sun's own pos_x binding.
                BindingDef {
                    id: "7_pos_x".to_string(),
                    label: String::new(),
                    default_value: 5.0,
                    target: BindingTarget::Node {
                        node_id: NodeId::new("sun"),
                        param: "pos_x".to_string(),
                    },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                    default_mirrors_node_param: false,
                },
                // D7 fan-out: the SAME id, a SECOND binding targeting the
                // envmap's sun-disc param — the leak vector.
                BindingDef {
                    id: "7_pos_x".to_string(),
                    label: String::new(),
                    default_value: 5.0,
                    target: BindingTarget::Node {
                        node_id: NodeId::new("envmap"),
                        param: "sun_x".to_string(),
                    },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                    default_mirrors_node_param: false,
                },
            ],
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        };
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: None,
            description: None,
            preset_metadata: Some(meta),
            scene_modifiers: Vec::new(),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    fn ids(names: &[&str]) -> Vec<NodeId> {
        names.iter().map(NodeId::new).collect()
    }

    #[test]
    fn world_sections_exclude_the_fanned_out_sun_section() {
        let def = azalea_like_fixture();
        let sections = sections_for_nodes(Some(&def), &ids(&["envmap"]));
        assert_eq!(
            sections,
            vec!["Environment".to_string()],
            "World must not pick up \"Sun\" via the sun's fanned-out envmap.sun_x binding"
        );
        assert_eq!(parameter_ids_for_nodes(Some(&def), &ids(&["envmap"])), vec!["1_intensity"]);
    }

    #[test]
    fn the_lights_own_item_still_includes_its_section() {
        let def = azalea_like_fixture();
        let sections = sections_for_nodes(Some(&def), &ids(&["sun"]));
        assert_eq!(sections, vec!["Sun".to_string()]);
        assert_eq!(parameter_ids_for_nodes(Some(&def), &ids(&["sun"])), vec!["7_pos_x"]);
    }

    #[test]
    fn custom_named_controls_resolve_through_their_primary_binding() {
        let mut def = azalea_like_fixture();
        let metadata = def.preset_metadata.as_mut().unwrap();
        metadata.params[1].id = "custom_amount".into();
        for binding in &mut metadata.bindings {
            if binding.id == "7_pos_x" { binding.id = "custom_amount".into(); }
        }
        assert_eq!(parameter_ids_for_nodes(Some(&def), &ids(&["sun"])), vec!["custom_amount"]);
        assert_eq!(parameter_ids_for_nodes(Some(&def), &ids(&["envmap"])), vec!["1_intensity"]);
        assert!(parameter_ids_for_nodes(Some(&def), &[]).is_empty());
    }

    /// A stamped id's numeric prefix is a group-local doc id: a control whose
    /// prefix matches another node's doc id still belongs to its target.
    #[test]
    fn a_colliding_stamp_prefix_names_no_owner() {
        let mut def = azalea_like_fixture();
        let metadata = def.preset_metadata.as_mut().unwrap();
        for binding in &mut metadata.bindings {
            if binding.id == "1_intensity" {
                binding.target = BindingTarget::Node { node_id: NodeId::new("grouped_domain"), param: "speed".into() };
            }
        }
        assert!(parameter_ids_for_nodes(Some(&def), &ids(&["envmap"])).is_empty());
        assert_eq!(parameter_ids_for_nodes(Some(&def), &ids(&["grouped_domain"])), vec!["1_intensity"]);
    }

    /// Items find their rows by the node they name, not by list order: the
    /// rows arrive reversed and every item still takes its own row's controls.
    #[test]
    fn scene_items_pair_with_rows_by_node_not_by_order() {
        use manifold_core::SceneNodeRef;
        use manifold_nodes_scene::node_graph::scene_items::{SceneItemId, scene_items};
        use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm};
        use manifold_ui::panels::scene_setup_panel::SceneSelection;
        let def = manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::new("WaterDamBreakGpuFlip")).unwrap();
        let vm = SceneVm::from_def(&def).expect("a scene");
        let items = scene_items(&def, &vm);
        let mut rows: Vec<(SceneNodeRef, Option<SceneNodeRef>, String)> = vm
            .objects
            .iter()
            .filter_map(|object| match object {
                SceneObjectVm::Known(row) => Some((row.address.clone(), row.parent.clone(), row.address.node.as_str().to_string())),
                SceneObjectVm::Custom { .. } => None,
            })
            .collect();
        assert!(rows.len() >= 2, "the fixture has several objects");
        rows.reverse();
        let objects: Vec<_> = rows.iter().map(|(node, parent, _)| (node.clone(), parent.clone())).collect();
        let object_ids: Vec<Vec<String>> = rows.iter().map(|(_, _, name)| vec![format!("controls-of-{name}")]).collect();
        let tree = scene_item_vms(Some(&def), &items, &objects, &[], object_ids, &|_| None);
        let mut paired = 0;
        for item in &items {
            let SceneItemId::Object(leaf) = &item.id else { continue };
            let shown = tree.iter().find(|shown| shown.name == item.name && matches!(&shown.selection, SceneSelection::Object(_))).expect("the object item is listed");
            assert!(matches!(&shown.selection, SceneSelection::Object(node) if &node.node == leaf), "{} selects its own row", item.name);
            let (_, _, name) = rows.iter().find(|(node, _, _)| &node.node == leaf).unwrap();
            assert_eq!(shown.parameter_ids, vec![format!("controls-of-{name}")], "{} takes its own row's controls", item.name);
            paired += 1;
        }
        assert!(paired >= 2);
    }

    /// Every scene item with the nodes it owns (`scene_items`), and the
    /// material it may share.
    fn scene_items(def: &EffectGraphDef) -> Vec<(String, Vec<NodeId>, Option<NodeId>)> {
        use manifold_nodes_scene::node_graph::scene_items::SceneItemId;
        use manifold_nodes_scene::node_graph::scene_vm::{MaterialVm, SceneObjectVm, SceneVm};
        let vm = SceneVm::from_def(def).expect("a scene");
        manifold_nodes_scene::node_graph::scene_items::scene_items(def, &vm)
            .into_iter()
            .map(|item| {
                let material = vm.objects.iter().find_map(|object| match (object, &item.id) {
                    (SceneObjectVm::Known(row), SceneItemId::Object(id)) if &row.object == id => match &row.material {
                        MaterialVm::Known(m) => Some(m.node.clone()),
                        _ => None,
                    },
                    _ => None,
                });
                (format!("{:?} {}", item.kind, item.name), item.owned_nodes, material)
            })
            .collect()
    }

    /// The nodes an object's scene item owns.
    fn object_nodes(def: &EffectGraphDef, row: &manifold_nodes_scene::node_graph::scene_vm::SceneObjectKnownRow) -> Vec<NodeId> {
        let vm = manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(def).expect("a scene");
        let items = manifold_nodes_scene::node_graph::scene_items::scene_items(def, &vm);
        owned_nodes(&items, &manifold_nodes_scene::node_graph::scene_items::SceneItemId::Object(row.object.clone())).to_vec()
    }

    /// Every control the scene panel shows belongs to one item: the camera, a
    /// light, an object, a physics world or the world. The one sharing allowed is a material
    /// wired into several objects, which each of them shows. Returns the
    /// camera's controls. Doc ids collide across group levels (Dam Break
    /// Matter's domain and its orbit camera are both doc id 1; every grouped
    /// glTF object numbers its own level), so this fails the moment
    /// ownership reads them.
    fn assert_one_owner(name: &str, def: &EffectGraphDef) -> Vec<String> {
        assert!(manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(def).is_some(), "{name} is a scene");
        let items = scene_items(def);
        let mut owner: BTreeMap<String, usize> = BTreeMap::new();
        for (index, (item, nodes, material)) in items.iter().enumerate() {
            for id in parameter_ids_for_nodes(Some(def), nodes) {
                let Some(other) = owner.insert(id.clone(), index) else { continue; };
                let shared_material = material.as_ref().filter(|material| {
                    items[other].2.as_ref() == Some(*material)
                        && parameter_ids_for_nodes(Some(def), std::slice::from_ref(*material)).contains(&id)
                });
                assert!(shared_material.is_some(),
                    "{name}: control {id} shows under both {} and {item}", items[other].0);
            }
        }
        let camera = items.iter().find(|(item, _, _)| item == "Camera Camera").expect("camera item");
        parameter_ids_for_nodes(Some(def), &camera.1)
    }

    /// SCENE_SURFACES_DESIGN.md P3 gate: every control of a bundled scene
    /// bound to one scene-vocabulary node param has exactly one owning item
    /// (`assert_one_owner`) and exactly one role, read from the joined
    /// manifest the scene table feeds.
    #[test]
    fn every_bundled_scene_row_has_one_owner_and_one_role() {
        use manifold_nodes_scene::node_graph::scene_exposure::is_scene_setup_node;
        fn node_types(nodes: &[manifold_core::effect_graph_def::EffectGraphNode], out: &mut std::collections::HashMap<NodeId, String>) {
            for node in nodes {
                out.insert(node.node_id.clone(), node.type_id.clone());
                if let Some(body) = node.group.as_deref() {
                    node_types(&body.nodes, out);
                }
            }
        }
        let mut checked = 0;
        let mut scenes = Vec::new();
        let mut failures = Vec::new();
        let mut ownerless = std::collections::BTreeSet::new();
        for preset in manifold_nodes::bundled_presets::bundled_preset_type_ids(
            manifold_core::preset_def::PresetKind::Generator,
        ) {
            let def = manifold_nodes::bundled_presets::bundled_preset_def(&preset).unwrap();
            if manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(def.as_ref()).is_none() {
                continue;
            }
            assert_one_owner(preset.as_str(), def.as_ref());
            scenes.push(preset.as_str().to_string());
            let owned: std::collections::BTreeSet<String> = scene_items(def.as_ref())
                .iter()
                .flat_map(|(_, nodes, _)| parameter_ids_for_nodes(Some(def.as_ref()), nodes))
                .collect();
            let mut fx = manifold_core::effects::PresetInstance::new_generator(preset.clone());
            fx.init_defaults();
            fx.graph = Some(def.as_ref().clone());
            fx.refresh_manifest_from_graph();
            let mut types = std::collections::HashMap::new();
            node_types(&def.nodes, &mut types);
            let meta = def.preset_metadata.as_ref().unwrap();
            for spec in &meta.params {
                let mut bound = meta.bindings.iter().filter(|b| b.id == spec.id);
                let (Some(binding), None) = (bound.next(), bound.next()) else { continue };
                let BindingTarget::Node { node_id, .. } = &binding.target else { continue };
                if binding.user_added || !types.get(node_id).is_some_and(|ty| is_scene_setup_node(ty)) {
                    continue;
                }
                checked += 1;
                if !owned.contains(&spec.id) {
                    ownerless.insert(format!("{} {}", preset.as_str(), spec.id));
                }
                if fx.params.get(&spec.id).is_none_or(|p| p.spec.scene.is_none()) {
                    failures.push(format!("{preset:?} {}: no role", spec.id));
                }
            }
        }
        assert!(scenes.iter().any(|scene| scene == "WaterDamBreakMatter"), "{scenes:?}");
        assert!(scenes.iter().any(|scene| scene == "WaterDamBreakGpuFlip"), "{scenes:?}");
        assert!(checked > 100, "only {checked} controls checked");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
        assert!(ownerless.is_empty(), "vocabulary rows no scene item owns: {ownerless:?}");
    }

    #[test]
    fn shared_transform_and_modifier_controls_follow_the_first_scene_consumer() {
        use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm};

        let mut def = azalea_like_fixture();
        def.nodes = [
            (1, "mesh", "node.cube_mesh"),
            (2, "shared_transform", "node.transform_3d"),
            (3, "shake", "node.transform_shake"),
            (4, "bend", "node.bend_mesh"),
            (5, "material", "node.pbr_material"),
            (6, "first", "node.scene_object"),
            (7, "second", "node.scene_object"),
            (8, "independent", "node.scene_object"),
            (9, "local_transform", "node.transform_3d"),
            (10, "scene", "node.render_scene"),
            (11, "output", "system.final_output"),
        ].into_iter().map(|(id, node_id, type_id)| {
            serde_json::from_value(serde_json::json!({
                "id": id, "nodeId": node_id, "typeId": type_id,
            })).unwrap()
        }).collect();
        def.nodes.iter_mut().find(|node| node.id == 10).unwrap().params.insert(
            "objects".into(),
            manifold_core::effect_graph_def::SerializedParamValue::Int { value: 3 },
        );
        def.wires = [
            (1, "vertices", 4, "in"),
            (2, "transform", 3, "transform"),
            (3, "out", 6, "transform"),
            (3, "out", 7, "transform"),
            (4, "out", 6, "vertices"),
            (4, "out", 7, "vertices"),
            (5, "out", 6, "material"),
            (5, "out", 7, "material"),
            (1, "vertices", 8, "vertices"),
            (9, "transform", 8, "transform"),
            (6, "object", 10, "object_0"),
            (7, "object", 10, "object_1"),
            (8, "object", 10, "object_2"),
            (10, "color", 11, "in"),
        ].into_iter().map(|(from_node, from_port, to_node, to_port)| EffectGraphWire {
            from_node, from_port: from_port.into(), to_node, to_port: to_port.into(),
        }).collect();
        let metadata = def.preset_metadata.as_mut().unwrap();
        metadata.params.clear();
        metadata.bindings.clear();
        for (id, node_id, param) in [
            ("position", "shared_transform", "pos_x"),
            ("shake_amount", "shake", "amount"),
            ("bend_angle", "bend", "angle"),
            ("roughness", "material", "roughness"),
            ("local_position", "local_transform", "pos_x"),
        ] {
            metadata.params.push(ParamSpecDef { id: id.into(), ..Default::default() });
            metadata.bindings.push(BindingDef {
                id: id.into(),
                target: BindingTarget::Node { node_id: NodeId::new(node_id), param: param.into() },
                ..binding_defaults()
            });
        }

        for first_object in [6, 7] {
            for wire in &mut def.wires {
                if wire.to_node == 10 && matches!(wire.from_node, 6 | 7) {
                    wire.to_port = if wire.from_node == first_object { "object_0" } else { "object_1" }.into();
                }
            }
            assert_one_owner("shared spatial controls", &def);
            let vm = SceneVm::from_def(&def).unwrap();
            for object in &vm.objects {
                let SceneObjectVm::Known(row) = object else { panic!("known object"); };
                let controls = parameter_ids_for_nodes(Some(&def), &object_nodes(&def, row));
                let expected = match row.index {
                    0 => vec!["position", "shake_amount", "bend_angle", "roughness"],
                    1 => vec!["roughness"],
                    2 => vec!["local_position"],
                    _ => unreachable!(),
                };
                assert_eq!(controls, expected, "{}", row.object);
                if row.index < 2 {
                    assert_eq!(row.transform.as_ref().unwrap().node, NodeId::new("shared_transform"));
                    assert_eq!(row.transform_chain.len(), 1);
                    assert_eq!(row.modifier_chain.len(), 1);
                }
            }
        }
    }

    #[test]
    fn the_matter_water_shows_no_camera_control() {
        use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm};
        let def = manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::new("WaterDamBreakMatter")).unwrap();
        let camera = assert_one_owner("WaterDamBreakMatter", def.as_ref());
        assert!(camera.iter().any(|id| id.ends_with("_distance")), "the orbit camera owns its dials: {camera:?}");
        let vm = SceneVm::from_def(def.as_ref()).unwrap();
        let water = vm.objects.iter().find_map(|object| match object {
            SceneObjectVm::Known(row) if row.liquid_domain.is_some() => Some(row),
            _ => None,
        }).expect("the water is a scene object");
        let sections = sections_for_nodes(Some(def.as_ref()), &object_nodes(def.as_ref(), water));
        assert!(sections.iter().any(|section| section.contains("Simulation")), "{sections:?}");
        assert!(sections.iter().all(|section| !section.contains("Camera")), "{sections:?}");
    }

    /// The inspector hides the Physics row when a Known object sits on a
    /// liquid domain and carries no physics of its own (BUG-ejcb, BUG-4lfm).
    /// The GPU FLIP water is such an object, so the row is keyed off these
    /// two facts, not off the solver's type id.
    #[test]
    fn gpu_flip_water_is_a_liquid_object_without_physics() {
        use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm};
        let def = manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::new("WaterDamBreakGpuFlip")).unwrap();
        let vm = SceneVm::from_def(def.as_ref()).expect("the GPU dam break is a scene");
        let water = vm.objects.iter().find_map(|object| match object {
            SceneObjectVm::Known(row) if row.liquid_domain.is_some() => Some(row),
            _ => None,
        }).expect("the GPU FLIP water is a Known scene object on a liquid domain");
        assert!(water.physics.is_none(), "water has no physics of its own: {}", water.name);
        let domain = water.liquid_domain.as_ref().unwrap();
        let flat = manifold_core::flatten::flatten_groups(def.as_ref())
            .expect("the GPU dam break scene flattens");
        let domain_type = flat.nodes.iter()
            .find(|node| node.node_id == domain.node)
            .map(|node| node.type_id.as_str());
        assert_eq!(domain_type, Some(manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID));
    }

    #[test]
    fn gpu_flip_obstacle_keeps_fluid_role_with_scene_modifier() {
        use manifold_core::{NodeId, SceneNodeRef, SceneNodeRefExt as _};
        use manifold_core::scene_modifier_preset::SceneTargetSelection;
        use manifold_nodes_scene::node_graph::{scene_modifier_authoring, scene_vm::SceneObjectVm, scene_vm::SceneVm};
        let mut def: manifold_core::effect_graph_def::EffectGraphDef = serde_json::from_str(
            manifold_nodes::testkit::assets::ASSETS_GENERATOR_PRESETS_WATERDAMBREAKGPUFLIP_JSON,
        ).unwrap();
        let scene = SceneNodeRef::locate(&def, &NodeId::new("scene")).unwrap();
        let water = SceneNodeRef::locate(&def, &NodeId::new("water_object")).unwrap();
        assert_eq!(water.scope.len(), 1);
        // The performer's modifier picker uses this same scoped object list.
        assert!(scene_modifier_authoring::scene_modifier_objects(&def, &scene).unwrap().contains(&water));
        let recipe = serde_json::from_str(manifold_nodes::testkit::assets::ASSETS_SCENE_MODIFIER_PRESETS_UNIFORMFORCE_JSON).unwrap();
        let instance = scene_modifier_authoring::prepare_new_scene_modifier(
            &def, &recipe, NodeId::new("force"), scene,
            SceneTargetSelection::Explicit { objects: vec![water] },
        ).unwrap();
        def = manifold_core::scene_modifier_edit::insert_scene_modifier(&def, 0, instance).unwrap().graph;
        assert_eq!(def.scene_modifiers.len(), 1);
        assert!(manifold_nodes_scene::node_graph::scene_exposure::migrate_scene_exposures(&mut def));
        let vm = SceneVm::from_def(&def).unwrap();
        let obstacle = vm.objects.iter().find_map(|object| match object {
            SceneObjectVm::Known(row) if row.name == "Obstacle" => Some(row),
            _ => None,
        }).unwrap();
        assert!(obstacle.group_node_id.is_some());
        let roles = fluid_role_rows(&def, obstacle.group_node_id, &fluid_domains(&vm)).unwrap();
        assert_eq!(roles.len(), 1, "the Fluid Role panel keeps the collider");
        assert_eq!(roles[0].target_label, "Target: Water");
        assert!(object_nodes(&def, obstacle).contains(&NodeId::new("obstacle_collider")));
    }

    /// The GPU water's panel carries its whitewater switch, amount and
    /// budget under one Whitewater section (BUG-ejcb item 2).
    #[test]
    fn gpu_flip_water_owns_its_whitewater_controls() {
        use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm};
        let def = manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::new("WaterDamBreakGpuFlip")).unwrap();
        assert_one_owner("WaterDamBreakGpuFlip", def.as_ref());
        let vm = SceneVm::from_def(def.as_ref()).unwrap();
        let water = vm.objects.iter().find_map(|object| match object {
            SceneObjectVm::Known(row) if row.liquid_domain.is_some() => Some(row),
            _ => None,
        }).expect("the water is a scene object");
        let owned = object_nodes(def.as_ref(), water);
        let ids = parameter_ids_for_nodes(Some(def.as_ref()), &owned);
        assert!(ids.iter().any(|id| id == "whitewater_capacity"), "missing whitewater budget");
        for param in ["enabled", "amount"] {
            let binding = def.preset_metadata.as_ref().unwrap().bindings.iter().find(|binding| {
                matches!(&binding.target, manifold_core::effect_graph_def::BindingTarget::Node {
                    node_id, param: target_param,
                } if node_id.as_str() == "whitewater" && target_param == param)
            }).expect("the whitewater stage exposes its control");
            assert!(ids.contains(&binding.id), "missing whitewater control {} in {ids:?}", binding.id);
        }
        let sections = sections_for_nodes(Some(def.as_ref()), &owned);
        assert!(sections.iter().any(|section| section == "Whitewater"), "{sections:?}");
    }

    /// Physics View is one card on the Water row: five bindings, the
    /// surface's two switches first, then the dots' visibility, instances
    /// and mesh, every one a switch selector.
    #[test]
    fn gpu_flip_water_row_shows_the_physics_view_card() {
        use manifold_core::effect_graph_def::BindingTarget;
        use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm};
        let def = manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::new("WaterDamBreakGpuFlip")).unwrap();
        let metadata = def.preset_metadata.as_ref().unwrap();
        let targets: Vec<_> = metadata.bindings.iter().filter(|binding| binding.id == "physics_view")
            .map(|binding| match &binding.target {
                BindingTarget::Node { node_id, param } => (node_id.as_str(), param.as_str()),
                target => panic!("physics_view targets {target:?}"),
            }).collect();
        assert_eq!(targets, [("water_vertices_switch", "selector"), ("water_indices_switch", "selector"),
            ("dots_visible", "selector"), ("dots_instances_switch", "selector"), ("dots_mesh_switch", "selector")]);
        let spec = metadata.params.iter().find(|spec| spec.id == "physics_view").expect("Physics View card");
        assert_eq!((spec.name.as_str(), spec.section.as_deref(), spec.is_toggle), ("Physics View", Some("Water"), true));
        let vm = SceneVm::from_def(def.as_ref()).unwrap();
        let rows: Vec<_> = vm.objects.iter().filter_map(|object| match object {
            SceneObjectVm::Known(row) => Some(row),
            _ => None,
        }).collect();
        let showing: Vec<_> = rows.iter().filter(|row| {
            let owned = object_nodes(def.as_ref(), row);
            let mut ids = parameter_ids_for_nodes(Some(def.as_ref()), &owned);
            filter_family_parameter_ids(Some(def.as_ref()), row, &mut ids);
            ids.iter().any(|id| id == "physics_view")
        }).map(|row| row.name.as_str()).collect();
        assert_eq!(showing, ["Water"]);
        assert_one_owner("WaterDamBreakGpuFlip", def.as_ref());
    }

    #[test]
    fn water_family_projection_keeps_parent_and_look_ownership() {
        use manifold_core::effect_graph_def::BindingTarget;
        use manifold_nodes_scene::node_graph::scene_vm::{MaterialVm, SceneObjectVm, SceneVm};

        for preset in ["WaterDamBreakGpuFlip", "WaterDamBreakParticles"] {
            let def = manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::new(preset))
                .expect("water family preset");
            let vm = SceneVm::from_def(def.as_ref()).expect("water family scene");
            let water = vm.objects.iter().find_map(|object| match object {
                SceneObjectVm::Known(row) if row.name == "Water" => Some(row),
                _ => None,
            }).expect("Water parent");
            assert!(water.is_group && water.parent.is_none(), "{preset}: real Water parent");

            // Collapsing or expanding the family leaves the parent's own
            // simulation and shared gate controls intact.
            let parent_owned = object_nodes(def.as_ref(), water);
            let mut parent_ids = parameter_ids_for_nodes(Some(def.as_ref()), &parent_owned);
            filter_family_parameter_ids(Some(def.as_ref()), water, &mut parent_ids);
            assert!(parent_ids.contains(&"parent_visible".to_string()), "{preset}: parent gate");
            assert!(sections_for_nodes(Some(def.as_ref()), &parent_owned).iter().any(|section| section == "Whitewater"),
                "{preset}: parent simulation controls");

            let children: Vec<_> = vm.objects.iter().filter_map(|object| match object {
                SceneObjectVm::Known(row) if row.parent.as_ref() == Some(&water.address) => Some(row),
                _ => None,
            }).collect();
            // The Dam Break adds Physics View's dots after the three looks.
            let physics_view = preset == "WaterDamBreakGpuFlip";
            assert_eq!(children.len(), 3 + usize::from(physics_view), "{preset}: look rows");
            let (dots, children): (Vec<_>, Vec<_>) = children.into_iter().partition(|child| child.name == "Physics Dots");
            for dots in dots {
                // Its switches belong to Water's Physics View card; the dots
                // row shows only its material.
                let owned = object_nodes(def.as_ref(), dots);
                let mut ids = parameter_ids_for_nodes(Some(def.as_ref()), &owned);
                filter_family_parameter_ids(Some(def.as_ref()), dots, &mut ids);
                let MaterialVm::Known(material) = &dots.material else { panic!("{preset}: dots material") };
                let metadata = def.preset_metadata.as_ref().unwrap();
                for id in &ids {
                    let binding = metadata.bindings.iter().find(|binding| &binding.id == id).expect("binding");
                    assert!(matches!(&binding.target, BindingTarget::Node { node_id, .. } if node_id == &material.node),
                        "{preset}: Physics Dots shows {id}");
                }
                assert!(dots.visible_driven, "{preset}: the card drives the dots' visibility");
            }
            assert_eq!(children.len(), 3, "{preset}: three look rows");
            for child in children {
                let material = match &child.material {
                    MaterialVm::Known(material) => material.node.clone(),
                    MaterialVm::None => panic!("{preset}: {} has no material", child.name),
                };
                let mesh = child.look_node().expect("look mesh");
                let owned = object_nodes(def.as_ref(), child);
                assert!(owned.contains(&child.object));
                assert!(owned.contains(&mesh));
                assert!(owned.contains(&material));
                assert!(child.transform.is_none() && child.transform_chain.is_empty());
                assert!(child.modifier_chain.is_empty() && child.physics.is_none());

                let mut ids = parameter_ids_for_nodes(Some(def.as_ref()), &owned);
                filter_family_parameter_ids(Some(def.as_ref()), child, &mut ids);
                let metadata = def.preset_metadata.as_ref().unwrap();
                let specs: Vec<_> = metadata.params.iter().filter(|spec| ids.contains(&spec.id)).collect();
                assert!(specs.iter().any(|spec| spec.name == "Visible"), "{preset}: {} eye", child.name);
                assert!(specs.iter().any(|spec| spec.name == "Size"), "{preset}: {} size", child.name);
                for spec in specs {
                    let binding = metadata.bindings.iter().find(|binding| binding.id == spec.id)
                        .expect("spec binding");
                    match &binding.target {
                        BindingTarget::Node { node_id, param } if node_id == &child.object => {
                            assert_eq!(param, "visible", "{preset}: {} object affordance", child.name);
                        }
                        BindingTarget::Node { node_id, param } if node_id == &mesh => {
                            assert_eq!(param, "radius", "{preset}: {} mesh affordance", child.name);
                        }
                        BindingTarget::Node { node_id, .. } if node_id == &material => {}
                        target => panic!("{preset}: {} leaked child target {target:?}", child.name),
                    }
                }
            }
        }
    }

    #[test]
    fn gpu_flip_water_surface_shape_sliders_route_from_water_detail() {
        use manifold_core::effect_graph_def::BindingTarget;
        use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm};
        let def = manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::new("WaterDamBreakGpuFlip")).unwrap();
        let vm = SceneVm::from_def(def.as_ref()).unwrap();
        let water = vm.objects.iter().find_map(|object| match object {
            SceneObjectVm::Known(row) if row.liquid_domain.is_some() => Some(row),
            _ => None,
        }).expect("water object");
        let owned = object_nodes(def.as_ref(), water);
        let specs: Vec<_> = owned_specs(Some(def.as_ref()), &owned).collect();
        let meta = def.preset_metadata.as_ref().unwrap();
        let mut instance = manifold_core::effects::PresetInstance::new(PresetTypeId::new("WaterDamBreakGpuFlip"));
        instance.params = manifold_core::params::ParamManifest::from_params(
            meta.params.iter().cloned().map(manifold_core::params::Param::bundled).collect());
        let surface = crate::ui_bridge::projection::cards::gen_params_to_surface(&instance, "water", None, &[],
            crate::ui_bridge::projection::cards::SurfaceVisibility::All, (manifold_core::Bpm(120.0), 4.0));
        for (id, label, node, param, default) in [
            ("surface_stretch", "Stretch", "liquid_blobs", "stretch", 1.0),
            ("surface_centre_smoothing", "Centre Smoothing", "liquid_blobs", "smoothing", 0.0),
            ("surface_fill_pits", "Fill Pits", "liquid_fill_pits", "value", 0.0),
        ] {
            let spec = specs.iter().find(|spec| spec.id == id).expect("water owns the slider");
            assert_eq!(spec.name, label);
            assert!(spec.tooltip.as_deref().is_some_and(|help| !help.is_empty()));
            let row = surface.rows.iter().find(|row| row.id == id).expect("manifest row");
            assert_eq!(row.spec.tooltip, spec.tooltip);
            assert_eq!(row.spec.section.as_deref(), Some("Water Detail"));
            assert_eq!(spec.section.as_deref(), Some("Water Detail"));
            assert_eq!(spec.default_value, default);
            assert!(!spec.is_toggle && !spec.whole_numbers);
            let binding = meta.bindings.iter().find(|binding| binding.id == id).unwrap();
            assert!(matches!(&binding.target, BindingTarget::Node { node_id, param: target }
                if node_id.as_str() == node && target == param));
        }
        for id in ["surface_particle_scale", "mesh_relaxation"] {
            assert!(specs.iter().any(|spec| spec.id == id && spec.section.as_deref() == Some("Water Detail")));
        }
    }

    #[test]
    fn imported_gltf_scene_controls_have_one_owner() {
        for fixture in ["cc0__oomurasaki_azalea_r._x_pulchrum.glb", "cc0___mushroom.glb"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/gltf").join(fixture);
            let (def, _) = manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph(&path)
                .unwrap_or_else(|e| panic!("{fixture}: {e}"));
            let camera = assert_one_owner(fixture, &def);
            assert!(!camera.is_empty(), "{fixture}: the camera owns its dials");
        }
    }

    #[test]
    fn duplicated_ground_binding_uses_its_clone_target_for_section_ownership() {
        let mut def = azalea_like_fixture();
        def.nodes = vec![
            EffectGraphNode {
                id: 7,
                node_id: NodeId::new("sun"),
                type_id: "node.light".to_string(),
                handle: Some("Ground".to_string()),
                params: BTreeMap::new(),
                exposed_params: BTreeSet::new(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
            EffectGraphNode {
                id: 107,
                node_id: NodeId::new("sun_clone"),
                type_id: "node.light".to_string(),
                handle: Some("Ground 2".to_string()),
                params: BTreeMap::new(),
                exposed_params: BTreeSet::new(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
        ];
        let meta = def.preset_metadata.as_mut().expect("fixture metadata");
        meta.params.push(ParamSpecDef {
            id: "7_pos_x_duplicate".to_string(),
            name: "Position X (Ground 2)".to_string(),
            section: Some("Ground 2 — Transform".to_string()),
            ..Default::default()
        });
        meta.bindings.push(BindingDef {
            id: "7_pos_x_duplicate".to_string(),
            label: String::new(),
            default_value: 5.0,
            target: BindingTarget::Node {
                node_id: NodeId::new("sun_clone"),
                param: "pos_x".to_string(),
            },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        });

        let sections = sections_for_nodes(Some(&def), &ids(&["sun_clone"]));
        assert_eq!(sections, vec!["Ground 2 — Transform".to_string()]);
        assert_eq!(
            parameter_ids_for_nodes(Some(&def), &ids(&["sun_clone"])),
            vec!["7_pos_x_duplicate"]
        );
        assert!(parameter_ids_for_nodes(Some(&def), &ids(&["sun"])).contains(&"7_pos_x".to_string()));
        assert!(!parameter_ids_for_nodes(Some(&def), &ids(&["sun"])).contains(&"7_pos_x_duplicate".to_string()));
    }

    fn source_driven_physics_fixture(
        grouped: bool,
        source_wired: bool,
    ) -> (EffectGraphDef, PhysicsVm) {
        let mut def = azalea_like_fixture();
        let body = serde_json::from_value::<EffectGraphNode>(serde_json::json!({
            "id": 10, "nodeId": "body", "typeId": "node.rigid_body"
        })).unwrap();
        let mesh = serde_json::from_value::<EffectGraphNode>(serde_json::json!({
            "id": 11, "nodeId": "mesh", "typeId": "node.cube_mesh"
        })).unwrap();
        let source_wire = EffectGraphWire {
            from_node: 11,
            from_port: "source".to_string(),
            to_node: 10,
            to_port: "source".to_string(),
        };
        if grouped {
            let group = EffectGraphNode {
                id: 40,
                node_id: NodeId::new("group"),
                type_id: "group".to_string(),
                handle: Some("Object".to_string()),
                params: BTreeMap::new(),
                exposed_params: BTreeSet::new(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: Some(Box::new(GroupDef {
                    tint: None,
                    interface: GroupInterface {
                        inputs: Vec::new(),
                        outputs: vec![InterfacePortDef {
                            name: "body".to_string(),
                            port_type: "RigidBody".to_string(),
                        }],
                        params: Vec::new(),
                    },
                    nodes: vec![body, mesh],
                    wires: source_wired.then_some(vec![source_wire]).unwrap_or_default(),
                })),
            };
            def.nodes = vec![group];
            def.wires = Vec::new();
        } else {
            def.nodes = vec![body, mesh];
            def.wires = source_wired.then_some(vec![source_wire]).unwrap_or_default();
        }
        let metadata = def.preset_metadata.as_mut().unwrap();
        metadata.params = [
            "10_shape",
            "10_collider_parts",
            "11_shape",
            "10_shape_duplicate",
            "11_shape_duplicate",
            "10_mixed_shape",
        ].into_iter().map(|id| ParamSpecDef {
            id: id.to_string(),
            name: id.to_string(),
            section: Some("Physics".to_string()),
            ..Default::default()
        }).collect();
        metadata.bindings = vec![
            BindingDef {
                id: "10_shape".to_string(),
                target: BindingTarget::Node { node_id: NodeId::new("body"), param: "shape".to_string() },
                ..binding_defaults()
            },
            BindingDef {
                id: "10_collider_parts".to_string(),
                target: BindingTarget::Node { node_id: NodeId::new("body"), param: "collider_parts".to_string() },
                ..binding_defaults()
            },
            BindingDef {
                id: "11_shape".to_string(),
                target: BindingTarget::Node { node_id: NodeId::new("mesh"), param: "shape".to_string() },
                ..binding_defaults()
            },
            BindingDef {
                id: "10_shape_duplicate".to_string(),
                target: BindingTarget::Node { node_id: NodeId::new("body"), param: "shape".to_string() },
                ..binding_defaults()
            },
            BindingDef {
                id: "11_shape_duplicate".to_string(),
                target: BindingTarget::Node { node_id: NodeId::new("mesh"), param: "shape".to_string() },
                ..binding_defaults()
            },
            BindingDef {
                id: "10_mixed_shape".to_string(),
                target: BindingTarget::Node { node_id: NodeId::new("body"), param: "shape".to_string() },
                ..binding_defaults()
            },
            BindingDef {
                id: "10_mixed_shape".to_string(),
                target: BindingTarget::Node { node_id: NodeId::new("mesh"), param: "shape".to_string() },
                ..binding_defaults()
            },
        ];
        let physics = PhysicsVm {
            body_node_id: 10,
            body: NodeId::new("body"),
            body_scope_path: grouped.then_some(vec![40]).unwrap_or_default(),
            enabled: true,
            imported: false,
        };
        (def, physics)
    }

    fn binding_defaults() -> BindingDef {
        BindingDef {
            id: String::new(),
            label: String::new(),
            default_value: 0.0,
            target: BindingTarget::Node {
                node_id: NodeId::new("unused"),
                param: String::new(),
            },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        }
    }

    #[test]
    fn source_driven_physics_projection_hides_only_inactive_body_geometry_controls() {
        let expected_ids = [
            "11_shape",
            "11_shape_duplicate",
            "10_mixed_shape",
        ];
        for grouped in [false, true] {
            let (def, physics) = source_driven_physics_fixture(grouped, true);
            let mut parameter_ids = parameter_ids_for_nodes(Some(&def), &ids(&["body", "mesh"]));
            filter_inactive_physics_parameter_ids(Some(&def), Some(&physics), &mut parameter_ids);
            assert_eq!(parameter_ids, expected_ids);

            let reloaded: EffectGraphDef = serde_json::from_str(
                &serde_json::to_string(&def).unwrap(),
            ).unwrap();
            let mut reloaded_ids = parameter_ids_for_nodes(Some(&reloaded), &ids(&["body", "mesh"]));
            filter_inactive_physics_parameter_ids(Some(&reloaded), Some(&physics), &mut reloaded_ids);
            assert_eq!(reloaded_ids, expected_ids, "reload preserves projection");
        }

        let (def, physics) = source_driven_physics_fixture(false, false);
        let mut parameter_ids = parameter_ids_for_nodes(Some(&def), &ids(&["body", "mesh"]));
        filter_inactive_physics_parameter_ids(Some(&def), Some(&physics), &mut parameter_ids);
        assert!(parameter_ids.contains(&"10_shape".to_string()));
        assert!(parameter_ids.contains(&"10_collider_parts".to_string()));
    }

    fn fluid_role_projection_fixture(grouped: bool, source_wired: bool) -> EffectGraphDef {
        let mut def = azalea_like_fixture();
        let role = serde_json::from_value::<EffectGraphNode>(serde_json::json!({
            "id": 20, "nodeId": "role", "typeId": "node.fluid_role_source"
        })).unwrap();
        let mesh = serde_json::from_value::<EffectGraphNode>(serde_json::json!({
            "id": 21, "nodeId": "mesh", "typeId": "node.cube_mesh"
        })).unwrap();
        let mesh_wire = EffectGraphWire {
            from_node: 21,
            from_port: "source".to_string(),
            to_node: 20,
            to_port: "mesh_0".to_string(),
        };
        if grouped {
            let inner = EffectGraphNode {
                id: 41,
                node_id: NodeId::new("nested_group"),
                type_id: "group".to_string(),
                handle: Some("Nested".to_string()),
                params: BTreeMap::new(),
                exposed_params: BTreeSet::new(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: Some(Box::new(GroupDef {
                    tint: None,
                    interface: GroupInterface { inputs: Vec::new(), outputs: Vec::new(), params: Vec::new() },
                    nodes: vec![role, mesh],
                    wires: source_wired.then_some(vec![mesh_wire]).unwrap_or_default(),
                })),
            };
            let outer = EffectGraphNode {
                id: 40,
                node_id: NodeId::new("object_group"),
                type_id: "group".to_string(),
                handle: Some("Object".to_string()),
                params: BTreeMap::new(),
                exposed_params: BTreeSet::new(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: Some(Box::new(GroupDef {
                    tint: None,
                    interface: GroupInterface { inputs: Vec::new(), outputs: Vec::new(), params: Vec::new() },
                    nodes: vec![inner],
                    wires: Vec::new(),
                })),
            };
            def.nodes = vec![outer];
            def.wires = Vec::new();
        } else {
            def.nodes = vec![role, mesh];
            def.wires = source_wired.then_some(vec![mesh_wire]).unwrap_or_default();
        }

        let ids = [
            "20_shape", "20_radius", "20_path", "20_mesh_index", "20_primitive_index",
            "20_material_index", "20_compound_materials", "20_geometry", "20_collider_parts",
            "21_shape", "20_mixed_shape", "20_shape_duplicate", "21_shape_duplicate",
        ];
        let metadata = def.preset_metadata.as_mut().unwrap();
        metadata.params = ids.iter().map(|id| ParamSpecDef {
            id: (*id).to_string(),
            name: (*id).to_string(),
            section: Some("Fluid Role".to_string()),
            ..Default::default()
        }).collect();
        let role_binding = |id: &str, param: &str| BindingDef {
            id: id.to_string(),
            target: BindingTarget::Node { node_id: NodeId::new("role"), param: param.to_string() },
            ..binding_defaults()
        };
        let mesh_binding = |id: &str| BindingDef {
            id: id.to_string(),
            target: BindingTarget::Node { node_id: NodeId::new("mesh"), param: "shape".to_string() },
            ..binding_defaults()
        };
        metadata.bindings = vec![
            role_binding("20_shape", "shape"),
            role_binding("20_radius", "radius"),
            role_binding("20_path", "path"),
            role_binding("20_mesh_index", "mesh_index"),
            role_binding("20_primitive_index", "primitive_index"),
            role_binding("20_material_index", "material_index"),
            role_binding("20_compound_materials", "compound_materials"),
            role_binding("20_geometry", "geometry"),
            role_binding("20_collider_parts", "collider_parts"),
            mesh_binding("21_shape"),
            role_binding("20_mixed_shape", "shape"),
            mesh_binding("20_mixed_shape"),
            role_binding("20_shape_duplicate", "shape"),
            mesh_binding("21_shape_duplicate"),
        ];
        def
    }

    #[test]
    fn mesh_wired_fluid_role_projection_hides_only_fallback_controls_direct_grouped_and_reloaded() {
        let expected_ids = [
            "20_geometry",
            "20_collider_parts",
            "21_shape",
            "20_mixed_shape",
            "21_shape_duplicate",
        ];
        for grouped in [false, true] {
            let def = fluid_role_projection_fixture(grouped, true);
            let group_id = grouped.then_some(40);
            let mut parameter_ids = parameter_ids_for_nodes(Some(&def), &ids(&["role", "mesh"]));
            filter_inactive_fluid_role_parameter_ids(Some(&def), group_id, &mut parameter_ids);
            assert_eq!(parameter_ids, expected_ids);

            let reloaded: EffectGraphDef = serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
            let mut reloaded_ids = parameter_ids_for_nodes(Some(&reloaded), &ids(&["role", "mesh"]));
            filter_inactive_fluid_role_parameter_ids(Some(&reloaded), group_id, &mut reloaded_ids);
            assert_eq!(reloaded_ids, expected_ids, "reload preserves role projection");
        }
    }

    #[test]
    fn unwired_fluid_role_projection_retains_fallback_controls() {
        let def = fluid_role_projection_fixture(true, false);
        let mut parameter_ids = parameter_ids_for_nodes(Some(&def), &ids(&["role", "mesh"]));
        let expected = parameter_ids.clone();
        filter_inactive_fluid_role_parameter_ids(Some(&def), Some(40), &mut parameter_ids);
        assert_eq!(parameter_ids, expected);
    }
}
