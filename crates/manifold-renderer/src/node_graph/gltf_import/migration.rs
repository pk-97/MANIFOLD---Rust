//! Load migrations for the glTF graph shapes emitted by this family.

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire, GroupDef, InterfacePortDef, SerializedParamValue, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID};

/// GLTF_ANIM_RUNTIME_V2_DESIGN.md D5: a project/preset saved before P2
/// baked keyframe/topology payload straight into the three glTF sampler
/// nodes' Table params (`node.gltf_skeleton_pose`'s six tables,
/// `node.gltf_animation_source`'s `translation_track`/`rotation_track`/
/// `scale_track`, `node.gltf_morph_weights`' `weight_tracks`). This strips
/// those dead Tables and stamps a `path` stringBinding from the SAME
/// group's sibling `node.gltf_mesh_source`/`node.gltf_skinned_mesh_source`
/// node, when one resolves — so the sampler reads from the shared
/// `gltf_anim_cache` instead. `target_node`/`skin_index` are NOT
/// recoverable from the old flat Tables (they never recorded which glTF
/// scene-node index the baked values came from) — the migration leaves
/// them at their existing default (0 unless the def already carries a
/// stamped value, the P1-era case for `skeleton_pose`'s `skin_index`),
/// documented residual limitation for a genuinely pre-P2 multi-object
/// asset, never silently wrong for the common single-object-per-group
/// case the importer has always produced. A node whose sibling path can't
/// be resolved keeps its tables — inert but present, per the round-trip
/// corollary (never a silent drop, never a parallel sampling path reading
/// old tables) — and one warning names it. Idempotent: a def carrying none
/// of the old table keys is untouched (`false`, no clone). Same
/// single-choke-point placement as `migrate_def_type_ids` — every loader
/// converges on `instantiate_def`.
fn migrate_gltf_anim_v2(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::BindingTarget;

    const POSE_KEYS: &[&str] = &[
        "joint_parent_table",
        "joint_root_world_table",
        "inverse_bind_table",
        "translation_tracks",
        "rotation_tracks",
        "scale_tracks",
    ];
    const ANIM_KEYS: &[&str] = &["translation_track", "rotation_track", "scale_track"];
    const MORPH_KEYS: &[&str] = &["weight_tracks"];

    fn old_table_keys(type_id: &str) -> Option<&'static [&'static str]> {
        match type_id {
            "node.gltf_skeleton_pose" => Some(POSE_KEYS),
            "node.gltf_animation_source" => Some(ANIM_KEYS),
            "node.gltf_morph_weights" => Some(MORPH_KEYS),
            _ => None,
        }
    }

    // node_id -> path default, for every stringBinding that already
    // targets SOME node's `path` param (sampler nodes stamped by P1+, and
    // every mesh-source-family node the importer has always stamped).
    let existing_paths: ahash::AHashMap<String, String> = def
        .preset_metadata
        .as_ref()
        .map(|m| {
            m.string_bindings
                .iter()
                .filter_map(|b| match &b.target {
                    BindingTarget::Node { node_id, param } if param == "path" => {
                        Some((node_id.as_str().to_string(), b.default_value.clone()))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    // Recurse scope-by-scope (top-level `def.nodes`, then each group body —
    // same "structurally identical at any depth" shape
    // `scene_object_migration::migrate_scope` uses): within one scope, a
    // mesh-source-family sibling's resolved path is this scope's fallback
    // for every sampler node in it that doesn't already have its own.
    #[allow(clippy::too_many_arguments)]
    fn migrate_scope(
        nodes: &mut [EffectGraphNode],
        existing_paths: &ahash::AHashMap<String, String>,
        can_add_binding: bool,
        new_bindings: &mut Vec<(String, String)>,
        changed: &mut bool,
        warnings: &mut Vec<String>,
    ) {
        let sibling_path: Option<&String> = nodes
            .iter()
            .find(|n| matches!(n.type_id.as_str(), "node.gltf_mesh_source" | "node.gltf_skinned_mesh_source"))
            .and_then(|n| existing_paths.get(n.node_id.as_str()));

        for n in nodes.iter_mut() {
            if let Some(keys) = old_table_keys(&n.type_id) {
                let carries_old = keys.iter().any(|k| n.params.contains_key(*k));
                if carries_old {
                    let node_id = n.node_id.as_str().to_string();
                    let own_path = existing_paths.get(&node_id);
                    // A NEW binding (the sibling-fallback case) can only be
                    // applied when `def.preset_metadata` exists to hold it
                    // — a per-instance override without metadata (see
                    // `PresetMetadata`'s doc comment) has nowhere to put
                    // one, so stripping the tables there would leave the
                    // node with neither payload nor a path. Own-path
                    // (already stamped, P1+) never needs a new binding.
                    let path = own_path.or(if can_add_binding { sibling_path } else { None });
                    match path {
                        Some(p) => {
                            for k in keys {
                                n.params.remove(*k);
                            }
                            if own_path.is_none() {
                                new_bindings.push((node_id, p.clone()));
                            }
                            *changed = true;
                        }
                        None => warnings.push(format!(
                            "{node_id} ({}): no sibling mesh-source path resolved — keyframe \
                             tables kept inert, not stripped",
                            n.type_id
                        )),
                    }
                }
            }
            if let Some(group) = &mut n.group {
                migrate_scope(&mut group.nodes, existing_paths, can_add_binding, new_bindings, changed, warnings);
            }
        }
    }

    let mut changed = false;
    let mut new_bindings = Vec::new();
    let mut warnings = Vec::new();
    let can_add_binding = def.preset_metadata.is_some();
    migrate_scope(&mut def.nodes, &existing_paths, can_add_binding, &mut new_bindings, &mut changed, &mut warnings);

    for w in &warnings {
        log::warn!("gltf_anim_v2 migration: {w}");
    }
    if !new_bindings.is_empty()
        && let Some(meta) = def.preset_metadata.as_mut()
    {
        for (node_id, path) in new_bindings {
            meta.string_bindings.push(manifold_core::effect_graph_def::StringBindingDef {
                id: format!("gltf_anim_v2_migrated_path_{node_id}"),
                label: "Model File".to_string(),
                default_value: path,
                target: BindingTarget::Node {
                    node_id: manifold_core::NodeId::new(&node_id),
                    param: "path".to_string(),
                },
            });
        }
    }

    changed
}

/// Structural fingerprint of the importer's pre-AM4 "Ambient Occlusion"
/// group, resolved by topology rather than fixed node ids (the importer
/// mints fresh ids per import, so a doc-order or id-based match would be
/// fragile). `None` if any required role is missing or ambiguous.
struct AoGroupRoles {
    group_in: u32,
    group_out: u32,
    ssao: u32,
    bilat_h: u32,
    bilat_v: u32,
    mix: u32,
}

fn identify_ao_group_roles(group: &GroupDef) -> Option<AoGroupRoles> {
    if group.nodes.len() != 6 {
        return None;
    }
    let group_in = group.nodes.iter().find(|n| n.type_id == GROUP_INPUT_TYPE_ID)?.id;
    let group_out = group.nodes.iter().find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)?.id;
    let ssao_ids: Vec<u32> =
        group.nodes.iter().filter(|n| n.type_id == "node.ssao_gtao").map(|n| n.id).collect();
    let bilat_ids: Vec<u32> =
        group.nodes.iter().filter(|n| n.type_id == "node.bilateral_blur").map(|n| n.id).collect();
    let mix_nodes: Vec<&EffectGraphNode> =
        group.nodes.iter().filter(|n| n.type_id == "node.mix").collect();
    if ssao_ids.len() != 1 || bilat_ids.len() != 2 || mix_nodes.len() != 1 {
        return None;
    }
    let ssao = ssao_ids[0];
    let mix_node = mix_nodes[0];
    // The pre-migration shape's multiply-only contract only holds when
    // `mode` is actually Multiply (enum value 4, CINEMATIC_POST D9) — a
    // hand-edited blend mode isn't the shape this migration owns.
    if !matches!(mix_node.params.get("mode"), Some(SerializedParamValue::Enum { value }) if *value == 4)
    {
        return None;
    }
    let mix = mix_node.id;

    let has = |from: u32, from_port: &str, to: u32, to_port: &str| {
        group.wires.iter().any(|w| {
            w.from_node == from && w.from_port == from_port && w.to_node == to && w.to_port == to_port
        })
    };

    // bilat_h / bilat_v are identified by topology (H feeds V), not by
    // handle name or declaration order.
    let bilat_h = *bilat_ids.iter().find(|&&b| has(ssao, "out", b, "in"))?;
    let bilat_v = *bilat_ids.iter().find(|&&b| b != bilat_h && has(bilat_h, "out", b, "in"))?;

    Some(AoGroupRoles { group_in, group_out, ssao, bilat_h, bilat_v, mix })
}

/// AM5 (RAYTRACING_DESIGN.md section 12, Screen-space AO handoff): does
/// `group` match the EXACT pre-Task-1 shape scene.rs's importer produced —
/// interface `[depth, camera, color] -> [out]`, six inner nodes (one
/// group-input, one group-output, one `node.ssao_gtao`, two
/// `node.bilateral_blur`, one `node.mix` at Multiply), wired in the fixed
/// 11-wire shape? Anything else — a hand edit, a different blend mode, an
/// already-migrated group (which carries a 4th interface input and a
/// `masked_mix` node, so it fails the interface-arity check below) — is
/// left byte-identical. A partial migration is worse than none.
fn ao_group_matches_pre_migration_shape(group: &GroupDef) -> bool {
    let expected_inputs = [("depth", "Texture2D"), ("camera", "Camera"), ("color", "Texture2D")];
    if group.interface.inputs.len() != expected_inputs.len()
        || group
            .interface
            .inputs
            .iter()
            .zip(expected_inputs.iter())
            .any(|(got, (name, ty))| got.name != *name || got.port_type != *ty)
    {
        return false;
    }
    if group.interface.outputs.len() != 1
        || group.interface.outputs[0].name != "out"
        || group.interface.outputs[0].port_type != "Texture2D"
    {
        return false;
    }
    if group.wires.len() != 11 {
        return false;
    }
    let Some(roles) = identify_ao_group_roles(group) else {
        return false;
    };

    let has = |from: u32, from_port: &str, to: u32, to_port: &str| {
        group.wires.iter().any(|w| {
            w.from_node == from && w.from_port == from_port && w.to_node == to && w.to_port == to_port
        })
    };
    has(roles.group_in, "depth", roles.ssao, "depth")
        && has(roles.group_in, "camera", roles.ssao, "camera")
        && has(roles.group_in, "color", roles.mix, "a")
        && has(roles.ssao, "out", roles.bilat_h, "in")
        && has(roles.group_in, "depth", roles.bilat_h, "depth")
        && has(roles.group_in, "camera", roles.bilat_h, "camera")
        && has(roles.bilat_h, "out", roles.bilat_v, "in")
        && has(roles.group_in, "depth", roles.bilat_v, "depth")
        && has(roles.group_in, "camera", roles.bilat_v, "camera")
        && has(roles.bilat_v, "out", roles.mix, "b")
        && has(roles.mix, "out", roles.group_out, "out")
}

/// Applies AM4's transformation to a group already confirmed by
/// [`ao_group_matches_pre_migration_shape`]: insert `node.masked_mix`,
/// gate the old multiply's output through it (`a` = the group's raw
/// `color` input, `b` = the multiply's result, `mask` = the new `ao_mask`
/// interface input), rewire the group output through `masked_mix`, and
/// remove the old direct `mix.out -> group_out.out` wire. `next_id` is
/// the shared, doc-wide id counter so a mint here can never collide with
/// a node minted in a sibling scope.
fn apply_ao_mask_migration(group: &mut GroupDef, next_id: &mut u32) {
    let roles = identify_ao_group_roles(group).expect("caller verified the shape matches");

    group.wires.retain(|w| {
        !(w.from_node == roles.mix
            && w.from_port == "out"
            && w.to_node == roles.group_out
            && w.to_port == "out")
    });

    let mask_mix_id = *next_id;
    *next_id += 1;
    group.nodes.push(EffectGraphNode {
        id: mask_mix_id,
        node_id: manifold_core::NodeId::default(),
        type_id: "node.masked_mix".to_string(),
        handle: Some("mask_mix".to_string()),
        params: Default::default(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: Default::default(),
        output_canvas_scales: Default::default(),
        group: None,
    });

    group.wires.push(EffectGraphWire {
        from_node: roles.group_in,
        from_port: "color".to_string(),
        to_node: mask_mix_id,
        to_port: "a".to_string(),
    });
    group.wires.push(EffectGraphWire {
        from_node: roles.mix,
        from_port: "out".to_string(),
        to_node: mask_mix_id,
        to_port: "b".to_string(),
    });
    group.wires.push(EffectGraphWire {
        from_node: roles.group_in,
        from_port: "ao_mask".to_string(),
        to_node: mask_mix_id,
        to_port: "mask".to_string(),
    });
    group.wires.push(EffectGraphWire {
        from_node: mask_mix_id,
        from_port: "out".to_string(),
        to_node: roles.group_out,
        to_port: "out".to_string(),
    });

    group.interface.inputs.push(InterfacePortDef {
        name: "ao_mask".to_string(),
        port_type: "Texture2D".to_string(),
    });
}

fn max_node_id_recursive_ao(nodes: &[EffectGraphNode]) -> u32 {
    nodes
        .iter()
        .map(|n| {
            let inner = n.group.as_ref().map(|g| max_node_id_recursive_ao(&g.nodes)).unwrap_or(0);
            n.id.max(inner)
        })
        .max()
        .unwrap_or(0)
}

fn migrate_ao_scope(
    nodes: &mut [EffectGraphNode],
    wires: &mut Vec<EffectGraphWire>,
    next_id: &mut u32,
    changed: &mut bool,
) {
    // Sibling `node.render_scene` ids in THIS scope, collected before any
    // mutation — AM5 requires the render_scene→group `color` wire to live
    // in the same scope as the group instance (the shape the importer
    // always produces: both at the top level of the scene graph).
    let render_scene_ids: Vec<u32> =
        nodes.iter().filter(|n| n.type_id == "node.render_scene").map(|n| n.id).collect();

    for node in nodes.iter_mut() {
        let node_id = node.id;
        if let Some(group) = node.group.as_deref_mut() {
            let feeding_render_scene = wires.iter().find(|w| {
                render_scene_ids.contains(&w.from_node)
                    && w.from_port == "color"
                    && w.to_node == node_id
                    && w.to_port == "color"
            });
            if let Some(render_scene_wire) = feeding_render_scene
                && ao_group_matches_pre_migration_shape(group)
            {
                let render_scene_id = render_scene_wire.from_node;
                apply_ao_mask_migration(group, next_id);
                wires.push(EffectGraphWire {
                    from_node: render_scene_id,
                    from_port: "ao_mask".to_string(),
                    to_node: node_id,
                    to_port: "ao_mask".to_string(),
                });
                *changed = true;
                log::warn!(
                    "gltf_ao_mask migration: group node {node_id} ('{}') migrated to consume \
                     render_scene's ao_mask output (RAYTRACING_DESIGN.md section 12, \
                     Screen-space AO handoff, AM5)",
                    node.handle.as_deref().unwrap_or("<unnamed>")
                );
            }
            migrate_ao_scope(&mut group.nodes, &mut group.wires, next_id, changed);
        }
    }
}

/// AM5 (RAYTRACING_DESIGN.md section 12, Screen-space AO handoff):
/// structure-gated migration for projects/presets saved before the
/// `ao_mask` consumer landed. Same single-choke-point placement as
/// `migrate_def_type_ids` and `migrate_gltf_anim_v2` — every loader
/// converges on `instantiate_def`. Idempotent by construction: a migrated
/// group carries a 4th interface input and a `masked_mix` node, so
/// `ao_group_matches_pre_migration_shape` can never match it again — no
/// version flag needed.
fn migrate_gltf_ao_mask(def: &mut EffectGraphDef) -> bool {
    let mut next_id = max_node_id_recursive_ao(&def.nodes) + 1;
    let mut changed = false;
    migrate_ao_scope(&mut def.nodes, &mut def.wires, &mut next_id, &mut changed);
    changed
}


inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "migrate_gltf_anim_v2",
        stage: manifold_node_engine::load::migration::MigrationStage::BeforeFlatten,
        order: 200,
        apply: migrate_gltf_anim_v2,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "migrate_gltf_ao_mask",
        stage: manifold_node_engine::load::migration::MigrationStage::BeforeFlatten,
        order: 210,
        apply: migrate_gltf_ao_mask,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare_node(id: u32, type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: manifold_core::NodeId::default(),
            type_id: type_id.to_string(),
            handle: None,
            params: Default::default(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        }
    }
    fn minimal_preset_metadata() -> manifold_core::effect_graph_def::PresetMetadata {
        manifold_core::effect_graph_def::PresetMetadata {
            id: manifold_core::PresetTypeId::new("test.migration_fixture"),
            display_name: "Migration Fixture".to_string(),
            category: "Spatial".to_string(),
            osc_prefix: "migration_fixture".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
                layer_types: None,
            params: Vec::new(),
            bindings: Vec::new(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
            scene_modifier: None,
            scene_bounds: None,
        }
    }
    fn old_table(rows: usize) -> manifold_core::effect_graph_def::SerializedParamValue {
        manifold_core::effect_graph_def::SerializedParamValue::Table {
            rows: (0..rows).map(|_| vec![0.0]).collect(),
        }
    }

    /// D5's core round trip: a pre-P2 rigid `node.gltf_animation_source`
    /// (old `translation_track`/`rotation_track` Tables, no `path`) sits
    /// alongside its sibling `node.gltf_mesh_source` INSIDE one group —
    /// the importer's own group-per-object shape. Migration must strip the
    /// old Tables, stamp a NEW `path` stringBinding pointing at the
    /// sibling's own resolved path, and stay idempotent on a second pass.
    #[test]
    fn migrate_gltf_anim_v2_strips_tables_and_stamps_sibling_path() {
        let mut mesh = bare_node(100, "node.gltf_mesh_source");
        mesh.node_id = manifold_core::NodeId::new("mesh_0");
        let mut anim = bare_node(101, "node.gltf_animation_source");
        anim.node_id = manifold_core::NodeId::new("anim_0");
        anim.params.insert("translation_track".to_string(), old_table(2));
        anim.params.insert("rotation_track".to_string(), old_table(2));

        let mut group = bare_node(0, manifold_core::effect_graph_def::GROUP_TYPE_ID);
        group.group = Some(Box::new(manifold_core::effect_graph_def::GroupDef {
            interface: manifold_core::effect_graph_def::GroupInterface {
                inputs: vec![],
                outputs: vec![],
                params: vec![],
            },
            nodes: vec![mesh, anim],
            wires: vec![],
            tint: None,
        }));

        let mut meta = minimal_preset_metadata();
        meta.string_bindings.push(manifold_core::effect_graph_def::StringBindingDef {
            id: "modelFile".to_string(),
            label: "Model File".to_string(),
            default_value: "/fixtures/old_shape.glb".to_string(),
            target: manifold_core::effect_graph_def::BindingTarget::Node {
                node_id: manifold_core::NodeId::new("mesh_0"),
                param: "path".to_string(),
            },
        });

        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: Some(meta),
            scene_modifiers: Vec::new(),
            nodes: vec![group],
            wires: vec![],
        };

        assert!(migrate_gltf_anim_v2(&mut def), "old-shape tables present -> migration must report changed");

        let inner = &def.nodes[0].group.as_ref().unwrap().nodes[1];
        assert_eq!(inner.type_id, "node.gltf_animation_source");
        assert!(!inner.params.contains_key("translation_track"), "old table must be stripped");
        assert!(!inner.params.contains_key("rotation_track"), "old table must be stripped");

        let anim_path_binding = def
            .preset_metadata
            .as_ref()
            .unwrap()
            .string_bindings
            .iter()
            .find(|b| matches!(&b.target, manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } if node_id.as_str() == "anim_0" && param == "path"))
            .expect("a NEW path stringBinding must be stamped for the sampler node");
        assert_eq!(anim_path_binding.default_value, "/fixtures/old_shape.glb");

        // Idempotence: a second pass over the already-migrated def is a no-op.
        assert!(!migrate_gltf_anim_v2(&mut def), "already-migrated def must report unchanged");
    }

    /// D5's inert-but-present branch: no sibling mesh-source path resolves
    /// (no `node.gltf_mesh_source`/`node.gltf_skinned_mesh_source` in
    /// scope) — the old Tables must survive UNTOUCHED, never a silent
    /// drop, and the function still reports `changed = false` (nothing was
    /// actually mutated).
    #[test]
    fn migrate_gltf_anim_v2_leaves_unresolvable_node_inert() {
        let mut anim = bare_node(0, "node.gltf_animation_source");
        anim.node_id = manifold_core::NodeId::new("anim_orphan");
        anim.params.insert("translation_track".to_string(), old_table(1));

        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: Some(minimal_preset_metadata()),
            scene_modifiers: Vec::new(),
            nodes: vec![anim],
            wires: vec![],
        };

        assert!(!migrate_gltf_anim_v2(&mut def), "no sibling path resolvable -> nothing changed");
        assert!(
            def.nodes[0].params.contains_key("translation_track"),
            "unresolvable node's tables must stay inert, never dropped"
        );
    }

    // ── RAYTRACING_DESIGN.md section 12 (Screen-space AO handoff), AM5 —
    // structure-gated migration of the importer's pre-ao_mask "Ambient
    // Occlusion" group ──

    /// Builds the EXACT pre-Task-1 group body scene.rs's importer produced:
    /// group_input(10) -> ssao_gtao(11) -> bilateral_blur(13, "H") ->
    /// bilateral_blur(14, "V") -> mix(12, Multiply) -> group_output(15),
    /// with `color` also feeding `mix.a` directly. Fixed local ids so
    /// tests can assert on the wire set without re-deriving it.
    fn pre_migration_ao_group_body() -> manifold_core::effect_graph_def::GroupDef {
        use manifold_core::effect_graph_def::{GroupDef, GroupInterface};

        let group_in = bare_node(10, GROUP_INPUT_TYPE_ID);
        let ssao = bare_node(11, "node.ssao_gtao");
        let mut mix = bare_node(12, "node.mix");
        mix.params.insert("mode".to_string(), SerializedParamValue::Enum { value: 4 });
        let bilat_h = bare_node(13, "node.bilateral_blur");
        let bilat_v = bare_node(14, "node.bilateral_blur");
        let group_out = bare_node(15, GROUP_OUTPUT_TYPE_ID);

        GroupDef {
            interface: GroupInterface {
                inputs: vec![
                    InterfacePortDef { name: "depth".to_string(), port_type: "Texture2D".to_string() },
                    InterfacePortDef { name: "camera".to_string(), port_type: "Camera".to_string() },
                    InterfacePortDef { name: "color".to_string(), port_type: "Texture2D".to_string() },
                ],
                outputs: vec![InterfacePortDef { name: "out".to_string(), port_type: "Texture2D".to_string() }],
                params: vec![],
            },
            nodes: vec![group_in, ssao, mix, bilat_h, bilat_v, group_out],
            wires: vec![
                EffectGraphWire { from_node: 10, from_port: "depth".to_string(), to_node: 11, to_port: "depth".to_string() },
                EffectGraphWire { from_node: 10, from_port: "camera".to_string(), to_node: 11, to_port: "camera".to_string() },
                EffectGraphWire { from_node: 10, from_port: "color".to_string(), to_node: 12, to_port: "a".to_string() },
                EffectGraphWire { from_node: 11, from_port: "out".to_string(), to_node: 13, to_port: "in".to_string() },
                EffectGraphWire { from_node: 10, from_port: "depth".to_string(), to_node: 13, to_port: "depth".to_string() },
                EffectGraphWire { from_node: 10, from_port: "camera".to_string(), to_node: 13, to_port: "camera".to_string() },
                EffectGraphWire { from_node: 13, from_port: "out".to_string(), to_node: 14, to_port: "in".to_string() },
                EffectGraphWire { from_node: 10, from_port: "depth".to_string(), to_node: 14, to_port: "depth".to_string() },
                EffectGraphWire { from_node: 10, from_port: "camera".to_string(), to_node: 14, to_port: "camera".to_string() },
                EffectGraphWire { from_node: 14, from_port: "out".to_string(), to_node: 12, to_port: "b".to_string() },
                EffectGraphWire { from_node: 12, from_port: "out".to_string(), to_node: 15, to_port: "out".to_string() },
            ],
            tint: None,
        }
    }

    /// Top-level def matching the importer's actual output: `node.render_scene`
    /// (id 1) feeding the pre-migration AO group (id 2) via `depth`/`camera`/
    /// `color`, same shape scene.rs's assembler wires at the outer level.
    fn pre_migration_ao_fixture() -> EffectGraphDef {
        use manifold_core::effect_graph_def::GROUP_TYPE_ID;

        let render_scene = bare_node(1, "node.render_scene");
        let mut ao_group = bare_node(2, GROUP_TYPE_ID);
        ao_group.group = Some(Box::new(pre_migration_ao_group_body()));

        EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![render_scene, ao_group],
            wires: vec![
                EffectGraphWire { from_node: 1, from_port: "depth".to_string(), to_node: 2, to_port: "depth".to_string() },
                EffectGraphWire { from_node: 1, from_port: "color".to_string(), to_node: 2, to_port: "color".to_string() },
            ],
        }
    }

    /// (I-AM1 core) A pre-migration def migrates to exactly the AM4 shape:
    /// `masked_mix` inserted, old direct `mix.out -> group_out.out` wire
    /// gone, 4th `ao_mask` interface input added, and the outer
    /// `render_scene.ao_mask -> group.ao_mask` wire added.
    #[test]
    fn migrate_gltf_ao_mask_migrates_pre_migration_group_to_expected_shape() {
        let mut def = pre_migration_ao_fixture();
        assert!(migrate_gltf_ao_mask(&mut def), "pre-migration shape must be recognized and migrated");

        let group = def.nodes[1].group.as_ref().expect("ao node still a group");
        assert_eq!(group.interface.inputs.len(), 4, "4th ao_mask interface input must be added");
        let ao_mask_input = &group.interface.inputs[3];
        assert_eq!(ao_mask_input.name, "ao_mask");
        assert_eq!(ao_mask_input.port_type, "Texture2D");

        assert_eq!(group.nodes.len(), 6 + 1, "masked_mix must be added, nothing else");
        let mask_mix = group
            .nodes
            .iter()
            .find(|n| n.type_id == "node.masked_mix")
            .expect("masked_mix node must exist");
        let mm = mask_mix.id;

        assert_eq!(group.wires.len(), 11 - 1 + 4, "old direct wire removed, 4 new wires added");
        let has = |from: u32, from_port: &str, to: u32, to_port: &str| {
            group.wires.iter().any(|w| {
                w.from_node == from && w.from_port == from_port && w.to_node == to && w.to_port == to_port
            })
        };
        assert!(has(10, "color", mm, "a"), "group_in.color -> mask_mix.a");
        assert!(has(12, "out", mm, "b"), "old mix.out -> mask_mix.b");
        assert!(has(10, "ao_mask", mm, "mask"), "group_in.ao_mask -> mask_mix.mask");
        assert!(has(mm, "out", 15, "out"), "mask_mix.out -> group_out.out");
        assert!(
            !has(12, "out", 15, "out"),
            "old direct mix.out -> group_out.out wire must be removed"
        );

        assert!(
            def.wires.iter().any(|w| w.from_node == 1
                && w.from_port == "ao_mask"
                && w.to_node == 2
                && w.to_port == "ao_mask"),
            "outer render_scene.ao_mask -> group.ao_mask wire must be added"
        );
    }

    /// I-AM1: migrating twice produces the same result as once — a migrated
    /// group carries a 4th interface input and a `masked_mix` node, so
    /// `ao_group_matches_pre_migration_shape` can never match it again.
    #[test]
    fn migrate_gltf_ao_mask_is_idempotent() {
        let mut def = pre_migration_ao_fixture();
        assert!(migrate_gltf_ao_mask(&mut def));
        let once = def.clone();

        assert!(!migrate_gltf_ao_mask(&mut def), "second pass over an already-migrated def is a no-op");
        assert_eq!(def, once, "migrate twice == migrate once");
    }

    /// A deliberately perturbed group (one `bilateral_blur` removed, so the
    /// shape no longer matches the importer's exact output) is left
    /// byte-identical — never a partial migration.
    #[test]
    fn migrate_gltf_ao_mask_leaves_perturbed_group_untouched() {
        let mut def = pre_migration_ao_fixture();
        {
            let group = def.nodes[1].group.as_mut().unwrap();
            group.nodes.retain(|n| n.id != 14);
            group.wires.retain(|w| w.from_node != 14 && w.to_node != 14);
        }
        let before = def.clone();

        assert!(!migrate_gltf_ao_mask(&mut def), "a hand-edited/perturbed group must not match");
        assert_eq!(def, before, "no partial migration — the def must stay byte-identical");
    }

}
