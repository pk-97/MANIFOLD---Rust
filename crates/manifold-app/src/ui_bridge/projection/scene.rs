//! Scene-panel projection: the Scene Setup dock's per-frame row value sync
//! and the exposure-section doc-id resolver. Moved from state_sync.rs (P-P,
//! UI_FUNNEL_DECOMPOSITION_DESIGN.md).

use manifold_core::project::Project;
use crate::ui_root::UIRoot;

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
    // params, resolved the SAME way `sync_card_values` resolves the main
    // generator inspector card's values (`ui_translate::with_param_slots`),
    // just against the SCENE PANEL's own bound layer (`live_layer_id`)
    // rather than the app's `active_layer` (a scene row always lives on the
    // layer its panel is docked to, which can differ from the app's active
    // layer — BUG-292).
    if let Some(gp) = gen_inst {
        crate::ui_translate::with_param_slots(&gp.params, |slots| {
            ui.scene_setup_panel.sync_properties_values(&mut ui.tree, slots)
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
    let Some(meta) = def.preset_metadata.as_ref() else { return Vec::new() };
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
        let Some(prefix_doc_id) = spec.id.split('_').next().and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if !doc_ids.contains(&prefix_doc_id) {
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

#[cfg(test)]
mod sections_for_doc_ids_tests {
    //! BUG-291: reproduces the exact glTF-importer fan-out shape
    //! (`gltf_import.rs`'s D7 sun-coherence block) that leaked a "Sun"
    //! section into World's item. `sections_for_doc_ids` is state_sync's
    //! own private fn — exercised directly (state-level, no pixels), per
    //! `docs/BUG_BACKLOG.md`'s prescribed fix shape.
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata, SkipModeDef,
        EFFECT_GRAPH_VERSION_WITH_METADATA,
    };
    use manifold_core::effects::ParamConvert;
    use manifold_core::NodeId;

    /// World = envmap (doc id 1) [+ atmosphere, omitted — not needed to
    /// reproduce the leak]. Sun = its own light node (doc id 7). The sun's
    /// `pos_x` control fans out to `envmap.sun_x` (D7 "sun coherence") under
    /// the SAME `id` as its own `sun.pos_x` binding — exactly the shape that
    /// made the old target-walking implementation attribute the fanned-out
    /// binding to World (whose doc-id set contains the envmap node the
    /// fan-out targets).
    fn azalea_like_fixture() -> EffectGraphDef {
        let meta = PresetMetadata {
            id: PresetTypeId::new("gltf_import_fixture"),
            display_name: "glTF Import Fixture".to_string(),
            category: "Diagnostic".to_string(),
            osc_prefix: "gltf_import_fixture".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
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
                    target: BindingTarget::Node { node_id: NodeId::new("envmap"), param: "intensity".to_string() },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                },
                // The sun's own pos_x binding.
                BindingDef {
                    id: "7_pos_x".to_string(),
                    label: String::new(),
                    default_value: 5.0,
                    target: BindingTarget::Node { node_id: NodeId::new("sun"), param: "pos_x".to_string() },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                },
                // D7 fan-out: the SAME id, a SECOND binding targeting the
                // envmap's sun-disc param — the leak vector.
                BindingDef {
                    id: "7_pos_x".to_string(),
                    label: String::new(),
                    default_value: 5.0,
                    target: BindingTarget::Node { node_id: NodeId::new("envmap"), param: "sun_x".to_string() },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                },
            ],
            skip_mode: SkipModeDef::default(),
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
    }

    #[test]
    fn the_lights_own_item_still_includes_its_section() {
        let def = azalea_like_fixture();
        // Sun's doc-id set: just its own light node (doc id 7).
        let sections = sections_for_doc_ids(Some(&def), &[7]);
        assert_eq!(sections, vec!["Sun".to_string()]);
    }
}
