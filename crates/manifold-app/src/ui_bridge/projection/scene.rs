//! Scene-panel projection: the Scene Setup dock's per-frame row value sync
//! and the exposure-section doc-id resolver. Moved from state_sync.rs (P-P,
//! UI_FUNNEL_DECOMPOSITION_DESIGN.md).

use crate::ui_root::UIRoot;
use manifold_core::project::Project;

/// Resolve a UI snapshot's document id into the stable graph address used by
/// content commands. Node ids are globally unique, including group bodies.
pub(crate) fn scene_node_ref_for_doc_id(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    wanted: u32,
) -> Option<manifold_core::scene_modifier_preset::SceneNodeRef> {
    fn visit(
        nodes: &[manifold_core::effect_graph_def::EffectGraphNode],
        wanted: u32,
        scope: &mut Vec<manifold_core::NodeId>,
    ) -> Option<manifold_core::scene_modifier_preset::SceneNodeRef> {
        for node in nodes {
            if node.id == wanted && !node.node_id.is_empty() {
                return Some(manifold_core::scene_modifier_preset::SceneNodeRef {
                    scope: scope.clone(), node: node.node_id.clone(),
                });
            }
            if let Some(group) = &node.group && !node.node_id.is_empty() {
                scope.push(node.node_id.clone());
                let found = visit(&group.nodes, wanted, scope);
                scope.pop();
                if found.is_some() { return found; }
            }
        }
        None
    }
    visit(&def.nodes, wanted, &mut Vec::new())
}

pub(crate) fn fluid_domains(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    scene: &manifold_renderer::node_graph::scene_vm::SceneVm,
) -> Vec<manifold_ui::panels::scene_setup_panel::FluidDomainOption> {
    fn is_domain(nodes: &[manifold_core::effect_graph_def::EffectGraphNode], id: u32) -> bool {
        nodes.iter().any(|node| (node.id == id && manifold_core::liquid_domain::is_liquid_domain(&node.type_id))
            || node.group.as_ref().is_some_and(|group| is_domain(&group.nodes, id)))
    }
    let mut result = Vec::new();
    for object in &scene.objects {
        let manifold_renderer::node_graph::scene_vm::SceneObjectVm::Known(row) = object else { continue; };
        for &id in &row.fluid_node_ids {
            if is_domain(&def.nodes, id)
                && !result.iter().any(|option: &manifold_ui::panels::scene_setup_panel::FluidDomainOption| option.node_doc_id == id)
            {
                result.push(manifold_ui::panels::scene_setup_panel::FluidDomainOption {
                    node_doc_id: id, name: row.name.clone(),
                });
            }
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
                [target] => domains.iter().find(|domain|
                    scene_node_ref_for_doc_id(def, domain.node_doc_id).as_ref() == Some(target))
                    .map(|domain| format!("Target: {}", domain.name))
                    .unwrap_or_else(|| "Fluid outside this scene".into()),
                targets => format!("Targets: {} fluids", targets.len()),
            };
            manifold_ui::panels::scene_setup_panel::FluidRoleRow {
                source_node_id: role.source_doc_id, name: role.name, target_label,
            }
        }).collect()
    })
}

pub(crate) fn group_fluid_role_ids(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    group_id: Option<u32>,
) -> Vec<u32> {
    fn collect(nodes: &[manifold_core::effect_graph_def::EffectGraphNode], ids: &mut Vec<u32>) {
        for node in nodes {
            if node.type_id == "node.fluid_role_source" {
                ids.push(node.id);
            }
            if let Some(group) = node.group.as_deref() {
                collect(&group.nodes, ids);
            }
        }
    }
    let Some(group) = group_id.and_then(|id| def.nodes.iter().find(|node| node.id == id))
        .and_then(|node| node.group.as_deref()) else { return Vec::new(); };
    let mut ids = Vec::new();
    collect(&group.nodes, &mut ids);
    ids
}

/// Resolve a modifier's controls through its scoped stable node identity.
/// The first binding owns a macro; secondary fan-out targets do not acquire
/// another copy of its UI. Custom exposed names need no numeric prefix.
pub(crate) fn object_modifier_parameter_ids(
    def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    group_id: Option<u32>,
    node_doc_id: u32,
) -> Vec<String> {
    use manifold_core::effect_graph_def::BindingTarget;
    let Some(def) = def else { return Vec::new(); };
    let Some(metadata) = &def.preset_metadata else { return Vec::new(); };
    let nodes = match group_id {
        Some(id) => {
            let Some(group) = def.nodes.iter().find(|node| node.id == id).and_then(|node| node.group.as_deref()) else {
                return Vec::new();
            };
            &group.nodes
        }
        None => &def.nodes,
    };
    let Some(node) = nodes.iter().find(|node| node.id == node_doc_id) else { return Vec::new(); };
    let identity = if node.node_id.is_empty() {
        manifold_core::NodeId::new(node.handle.clone().unwrap_or_else(|| format!("node{node_doc_id}")))
    } else { node.node_id.clone() };
    metadata.params.iter().filter(|param| {
        metadata.bindings.iter().find(|binding| binding.id == param.id)
            .is_some_and(|binding| matches!(&binding.target,
                BindingTarget::Node { node_id, .. } if node_id == &identity))
    }).map(|param| param.id.clone()).collect()
}

/// Per-frame VALUE sync for the Scene Setup dock's rows — the scene-row
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

/// P2 slice 2a (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): the REAL section
/// string(s) P1 stamped onto every param whose PRIMARY node is one of
/// `doc_ids` — read directly off `def`'s exposure metadata. Two stamping
/// code paths (creation-time commands vs the load-time migration) produce
/// DIFFERENT section strings for the same node kind (e.g. a scene_object's
/// own section is the bare handle at creation, "{handle} — Object" after
/// migration) — reading the real string is the only way to filter correctly
/// regardless of which path produced it. Dedups, preserves first-seen order.
///
/// BUG-291 (fixed): the original implementation attributed a param by
/// walking `meta.bindings` to each binding's TARGET node and checking that
/// against `doc_ids` — but a fan-out control (the glTF importer's D7 sun
/// macro: the sun's `pos_x/y/z` ALSO binds `envmap.sun_x/y/z` so one slider
/// drives both; similarly env intensity also drives `hdri_gain.gain`) adds
/// an EXTRA `BindingDef` under the SAME `id` targeting the OTHER node. Target-
/// walking misattributed those extra bindings to whichever item owned the
/// fanned-out-to node (World's `envmap` doc id matched the sun's `pos_x`
/// binding's target, so a "Sun" section leaked into World). Attributing by
/// the doc-id PREFIX of the param's OWN `id` instead is fan-out-proof: P1
/// stamps every exposed id as `{primary_node_doc_id}_{param}`
/// (`manifold_core::scene_exposure::stamp_scene_node_exposures_into`,
/// mirrored by the glTF importer's own hand-authored fan-out ids at
/// `gltf_import.rs`'s D7 block) — the prefix names the param's ONE true
/// owner regardless of how many nodes its value also happens to drive, so no
/// binding-target walk (and no node-doc-id cross-reference) is needed at
/// all.
pub(crate) fn sections_for_doc_ids(
    def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    doc_ids: &[u32],
) -> Vec<String> {
    let Some(def) = def else { return Vec::new() };
    let Some(meta) = def.preset_metadata.as_ref() else {
        return Vec::new();
    };
    if doc_ids.is_empty() {
        return Vec::new();
    }

    // Deliberately does NOT filter by `spec.card_visible`: the scene panel
    // keeps every P1-stamped param regardless of the CARD-curation flag (the
    // Scene Setup dock's own hand-curated `SceneVm` row builders in
    // `ui_bridge::projection::inspector`, not this section list, decide what
    // the panel shows) — `card_visible` only gates the generator/effect
    // outer CARD's row builder (`cards::param_surface`).
    let mut sections: Vec<String> = Vec::new();
    for spec in &meta.params {
        let owned = parameter_owned_by_doc_ids(def, meta, &spec.id, doc_ids);
        if !owned {
            continue;
        }
        let Some(section) = spec.section.clone() else {
            continue;
        };
        if !sections.contains(&section) {
            sections.push(section);
        }
    }
    sections
}

/// Project the exact exposed parameter ids owned by the supplied scene nodes.
/// Keep this predicate shared with [`sections_for_doc_ids`]: ordinary stamped
/// ids are owned by their numeric prefix, while cloned `_duplicate` ids are
/// resolved through their exact binding target. Curated names use their first
/// binding as owner; secondary fan-out targets do not acquire ownership.
pub(crate) fn parameter_ids_for_doc_ids(
    def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    doc_ids: &[u32],
) -> Vec<String> {
    let Some(def) = def else { return Vec::new() };
    let Some(meta) = def.preset_metadata.as_ref() else {
        return Vec::new();
    };
    if doc_ids.is_empty() {
        return Vec::new();
    }
    meta.params
        .iter()
        .filter(|spec| parameter_owned_by_doc_ids(def, meta, &spec.id, doc_ids))
        .map(|spec| spec.id.clone())
        .collect()
}

/// Remove only the legacy rigid-body geometry controls that are inactive when
/// a body is driven by a mesh source. `parameter_ids` is already owned by the
/// scene row's original projection predicate; this helper only subtracts
/// bindings that target the exact scoped body node and have no effective
/// target elsewhere.
pub(crate) fn filter_inactive_physics_parameter_ids(
    def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
    physics: Option<&manifold_renderer::node_graph::scene_vm::PhysicsVm>,
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

fn parameter_owned_by_doc_ids(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    meta: &manifold_core::effect_graph_def::PresetMetadata,
    parameter_id: &str,
    doc_ids: &[u32],
) -> bool {
    // A cloned scene binding retains its source numeric prefix and adds
    // `_duplicate` (or `_duplicate_N`). Resolve only these IDs through their
    // exact binding target; ordinary IDs stay prefix-based so the BUG-291
    // fan-out path cannot leak sections by target walking.
    if parameter_id.contains("_duplicate") {
        return meta
            .bindings
            .iter()
            .filter(|binding| binding.id == parameter_id)
            .any(|binding| match &binding.target {
                manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. } => {
                    doc_id_for_node_id(&def.nodes, node_id)
                        .is_some_and(|owner_doc_id| doc_ids.contains(&owner_doc_id))
                }
                _ => false,
            });
    }
    if let Some(prefix_doc_id) = parameter_id
        .split('_')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
    {
        return doc_ids.contains(&prefix_doc_id);
    }
    // Curated controls have ordinary names such as `resolution`, rather
    // than stamped numeric prefixes. Use the same primary-binding owner
    // as object_modifier_parameter_ids; secondary fan-out targets do not
    // acquire the control or its section.
    meta.bindings.iter().find(|binding| binding.id == parameter_id)
        .is_some_and(|binding| match &binding.target {
            manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. } => {
                doc_id_for_node_id(&def.nodes, node_id)
                    .is_some_and(|owner| doc_ids.contains(&owner))
            }
            _ => false,
        })
}

fn doc_id_for_node_id(
    nodes: &[manifold_core::effect_graph_def::EffectGraphNode],
    wanted: &manifold_core::NodeId,
) -> Option<u32> {
    for node in nodes {
        if &node.node_id == wanted {
            return Some(node.id);
        }
        if let Some(group) = node.group.as_deref()
            && let Some(doc_id) = doc_id_for_node_id(&group.nodes, wanted)
        {
            return Some(doc_id);
        }
    }
    None
}

#[cfg(test)]
mod sections_for_doc_ids_tests {
    //! BUG-291: reproduces the exact glTF-importer fan-out shape
    //! (`gltf_import.rs`'s D7 sun-coherence block) that leaked a "Sun"
    //! section into World's item. `sections_for_doc_ids` is state_sync's
    //! own private fn — exercised directly (state-level, no pixels), per
    //! `docs/BUG_BACKLOG.md`'s prescribed fix shape.
    use super::*;
    use manifold_core::NodeId;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef,
        EffectGraphNode, EffectGraphWire, GroupDef, GroupInterface, InterfacePortDef,
        ParamSpecDef, PresetMetadata,
    };
    use manifold_core::effects::ParamConvert;
    use manifold_renderer::node_graph::scene_vm::PhysicsVm;
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

    #[test]
    fn world_sections_exclude_the_fanned_out_sun_section() {
        let def = azalea_like_fixture();
        // World's doc-id set: just the envmap node (doc id 1).
        let sections = sections_for_doc_ids(Some(&def), &[1]);
        assert_eq!(
            sections,
            vec!["Environment".to_string()],
            "World must not pick up \"Sun\" via the sun's fanned-out envmap.sun_x binding"
        );
        assert_eq!(parameter_ids_for_doc_ids(Some(&def), &[1]), vec!["1_intensity"]);
    }

    #[test]
    fn the_lights_own_item_still_includes_its_section() {
        let def = azalea_like_fixture();
        // Sun's doc-id set: just its own light node (doc id 7).
        let sections = sections_for_doc_ids(Some(&def), &[7]);
        assert_eq!(sections, vec!["Sun".to_string()]);
        assert_eq!(parameter_ids_for_doc_ids(Some(&def), &[7]), vec!["7_pos_x"]);
    }

    #[test]
    fn object_modifier_controls_use_custom_binding_identity_without_fanout_leaks() {
        let mut def = azalea_like_fixture();
        def.nodes = vec![
            serde_json::from_value(serde_json::json!({
                "id": 7, "nodeId": "sun", "typeId": "node.bend_mesh"
            })).unwrap(),
            serde_json::from_value(serde_json::json!({
                "id": 1, "nodeId": "envmap", "typeId": "node.twist_mesh"
            })).unwrap(),
        ];
        let metadata = def.preset_metadata.as_mut().unwrap();
        metadata.params[1].id = "custom_amount".into();
        for binding in &mut metadata.bindings {
            if binding.id == "7_pos_x" { binding.id = "custom_amount".into(); }
        }
        assert_eq!(object_modifier_parameter_ids(Some(&def), None, 7), vec!["custom_amount"]);
        assert_eq!(object_modifier_parameter_ids(Some(&def), None, 1), vec!["1_intensity"]);
        assert!(object_modifier_parameter_ids(Some(&def), Some(404), 7).is_empty());
        assert_eq!(parameter_ids_for_doc_ids(Some(&def), &[7]), vec!["custom_amount"]);
        assert_eq!(parameter_ids_for_doc_ids(Some(&def), &[1]), vec!["1_intensity"]);
    }

    #[test]
    fn dam_break_water_owns_curated_quality_controls_after_migration_and_reload() {
        let mut def: EffectGraphDef = serde_json::from_str(include_str!(
            "../../../../manifold-renderer/assets/generator-presets/WaterDamBreak.json"
        )).unwrap();
        manifold_renderer::node_graph::scene_exposure::migrate_scene_exposures(&mut def);
        let def: EffectGraphDef = serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
        let ids = parameter_ids_for_doc_ids(Some(&def), &[4]);
        for id in ["resolution", "surface_detail", "whitewater", "surface_particle_scale", "4_grid_budget_mcells"] {
            assert!(ids.iter().any(|actual| actual == id), "missing Water control {id}");
        }
        assert!(!ids.iter().any(|id| id == "environment_mode"));
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

        let sections = sections_for_doc_ids(Some(&def), &[107]);
        assert_eq!(sections, vec!["Ground 2 — Transform".to_string()]);
        assert_eq!(
            parameter_ids_for_doc_ids(Some(&def), &[107]),
            vec!["7_pos_x_duplicate"]
        );
        assert!(parameter_ids_for_doc_ids(Some(&def), &[7]).contains(&"7_pos_x".to_string()));
        assert!(!parameter_ids_for_doc_ids(Some(&def), &[7]).contains(&"7_pos_x_duplicate".to_string()));
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
            let mut parameter_ids = parameter_ids_for_doc_ids(Some(&def), &[10, 11]);
            filter_inactive_physics_parameter_ids(Some(&def), Some(&physics), &mut parameter_ids);
            assert_eq!(parameter_ids, expected_ids);

            let reloaded: EffectGraphDef = serde_json::from_str(
                &serde_json::to_string(&def).unwrap(),
            ).unwrap();
            let mut reloaded_ids = parameter_ids_for_doc_ids(Some(&reloaded), &[10, 11]);
            filter_inactive_physics_parameter_ids(Some(&reloaded), Some(&physics), &mut reloaded_ids);
            assert_eq!(reloaded_ids, expected_ids, "reload preserves projection");
        }

        let (def, physics) = source_driven_physics_fixture(false, false);
        let mut parameter_ids = parameter_ids_for_doc_ids(Some(&def), &[10, 11]);
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
            assert_eq!(group_fluid_role_ids(&def, group_id), if grouped { vec![20] } else { Vec::new() });
            let mut parameter_ids = parameter_ids_for_doc_ids(Some(&def), &[20, 21]);
            filter_inactive_fluid_role_parameter_ids(Some(&def), group_id, &mut parameter_ids);
            assert_eq!(parameter_ids, expected_ids);

            let reloaded: EffectGraphDef = serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
            let mut reloaded_ids = parameter_ids_for_doc_ids(Some(&reloaded), &[20, 21]);
            filter_inactive_fluid_role_parameter_ids(Some(&reloaded), group_id, &mut reloaded_ids);
            assert_eq!(reloaded_ids, expected_ids, "reload preserves role projection");
        }
    }

    #[test]
    fn unwired_fluid_role_projection_retains_fallback_controls() {
        let def = fluid_role_projection_fixture(true, false);
        let mut parameter_ids = parameter_ids_for_doc_ids(Some(&def), &[20, 21]);
        let expected = parameter_ids.clone();
        filter_inactive_fluid_role_parameter_ids(Some(&def), Some(40), &mut parameter_ids);
        assert_eq!(parameter_ids, expected);
    }
}
