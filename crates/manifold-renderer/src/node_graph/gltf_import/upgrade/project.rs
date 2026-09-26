//! Project-wide material graph upgrades.
//!
//! This is the load/apply seam for the glTF material compatibility pass.  It
//! deliberately walks the serialized project rather than the live catalog: an
//! embedded definition is authoritative for its own instances, and an inline
//! instance graph is authoritative for that instance.

use std::collections::HashMap;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;

use super::{
    MaterialBindingUpdate, MaterialGraphUpgrade, MaterialUpgradeCache, upgrade_material_graph,
};

#[cfg(test)]
#[path = "project_tests.rs"]
mod project_tests;

/// Summary returned to the project loader.  Notices are user-facing but
/// non-fatal; a missing source asset must never make the project disappear.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MaterialUpgradeReport {
    pub changed_graphs: usize,
    pub notices: Vec<String>,
}

/// Upgrade all material graphs owned by `project` in one cache session.
///
/// The cache is intentionally shared across embedded definitions, instance
/// overrides, and scene modifier graphs.  This keeps repeated references to a
/// single imported asset cheap while retaining the graph-local metadata and
/// parameter ownership rules of [`upgrade_material_graph`].
pub fn upgrade_project_materials(project: &mut Project) -> MaterialUpgradeReport {
    let mut cache = MaterialUpgradeCache::default();
    let mut report = MaterialUpgradeReport::default();
    let mut embedded_updates: HashMap<String, Vec<MaterialBindingUpdate>> = HashMap::new();
    let mut embedded_defs: HashMap<String, EffectGraphDef> = HashMap::new();

    for embedded in &mut project.embedded_presets {
        let id = embedded.id().map(|id| id.as_str().to_owned());
        let result = upgrade_def(&mut embedded.def, &mut cache, &mut report);
        if let Some(id) = id {
            embedded_updates
                .entry(id.clone())
                .or_default()
                .extend(result.binding_updates);
            if result.changed {
                embedded_defs.insert(id, embedded.def.clone());
            }
        }
    }

    for instance in &mut project.settings.master_effects {
        upgrade_instance(
            instance,
            &embedded_updates,
            &embedded_defs,
            &mut cache,
            &mut report,
        );
    }
    for layer in &mut project.timeline.layers {
        if let Some(effects) = layer.effects.as_mut() {
            for instance in effects {
                upgrade_instance(
                    instance,
                    &embedded_updates,
                    &embedded_defs,
                    &mut cache,
                    &mut report,
                );
            }
        }
        if let Some(generator) = layer.gen_params_mut() {
            upgrade_instance(
                generator,
                &embedded_updates,
                &embedded_defs,
                &mut cache,
                &mut report,
            );
        }
        for clip in &mut layer.clips {
            for instance in &mut clip.effects {
                upgrade_instance(
                    instance,
                    &embedded_updates,
                    &embedded_defs,
                    &mut cache,
                    &mut report,
                );
            }
        }
    }

    report
}

fn upgrade_instance(
    instance: &mut PresetInstance,
    embedded_updates: &HashMap<String, Vec<MaterialBindingUpdate>>,
    embedded_defs: &HashMap<String, EffectGraphDef>,
    cache: &mut MaterialUpgradeCache,
    report: &mut MaterialUpgradeReport,
) {
    if let Some(graph) = instance.graph.as_mut() {
        let result = upgrade_def(graph, cache, report);
        if result.changed {
            // The graph's metadata is now the descriptor authority.  This
            // rebuild preserves values, exposure, calibration, and ordering
            // from the current manifest while adopting corrected defaults.
            instance.refresh_manifest_from_graph();
            instance.bump_graph_structure_version();
        }
        apply_binding_updates(instance, &result.binding_updates);
        return;
    }

    let id = instance.effect_type().as_str().to_owned();
    if let Some(def) = embedded_defs.get(&id) {
        // Graph-less instances normally get their descriptor from the live
        // catalog. Temporarily attaching the upgraded embedded definition
        // lets the existing manifest merge add new controls while preserving
        // the instance's graph-none topology and all existing values.
        instance.graph = Some(def.clone());
        instance.refresh_manifest_from_graph();
        instance.graph = None;
    }
    if let Some(updates) = embedded_updates.get(&id) {
        // A graph-less instance resolves its descriptor through the catalog
        // overlay.  Keep its live manifest in sync without replacing a value
        // that the user, or an active modulation source, owns.
        apply_binding_updates(instance, updates);
    }
}

fn upgrade_def(
    def: &mut EffectGraphDef,
    cache: &mut MaterialUpgradeCache,
    report: &mut MaterialUpgradeReport,
) -> MaterialGraphUpgrade {
    let mut result = upgrade_material_graph(def, cache);
    report.notices.extend(result.notices.iter().cloned());

    // Scene modifier graphs are persisted inside their owner graph and do not
    // have a PresetInstance manifest of their own.  They still need the same
    // source-path and cache treatment as the outer graph.
    for modifier in &mut def.scene_modifiers {
        let nested = upgrade_def(&mut modifier.graph, cache, report);
        result.changed |= nested.changed;
    }
    if contains_pbr_material(def) {
        result.changed |= crate::node_graph::scene_exposure::migrate_scene_exposures(def);
    }
    if result.changed {
        report.changed_graphs += 1;
    }
    result
}

fn contains_pbr_material(def: &EffectGraphDef) -> bool {
    fn nodes_contain(nodes: &[manifold_core::effect_graph_def::EffectGraphNode]) -> bool {
        nodes.iter().any(|node| {
            matches!(
                node.type_id.as_str(),
                "node.pbr_material" | "node.unlit_material"
            ) || node
                .group
                .as_ref()
                .is_some_and(|group| nodes_contain(&group.nodes))
        })
    }
    nodes_contain(&def.nodes)
        || def
            .scene_modifiers
            .iter()
            .any(|modifier| contains_pbr_material(&modifier.graph))
}

fn apply_binding_updates(instance: &mut PresetInstance, updates: &[MaterialBindingUpdate]) {
    for update in updates {
        let Some(default_value) = instance
            .params
            .get(update.id.as_str())
            .map(|param| param.spec.default_value)
        else {
            continue;
        };
        let base = instance.get_base_param(&update.id);
        let default_matches = nearly_equal(base, update.old_value);
        let owned = has_active_owner(instance, &update.id);

        if default_matches && !owned {
            instance.set_base_param_from_automation(&update.id, update.new_value);
        }

        // The descriptor default is safe to correct even when a custom base
        // value is retained.  Guarding on the old value avoids overwriting a
        // later calibration or a user-added descriptor.
        if nearly_equal(default_value, update.old_value)
            && let Some(param) = instance.params.get_mut(&update.id)
        {
            param.spec.default_value = update.new_value;
        }
    }
}

fn has_active_owner(instance: &PresetInstance, id: &str) -> bool {
    instance
        .drivers
        .iter()
        .flatten()
        .any(|driver| driver.param_id == id && driver.enabled)
        || instance
            .envelopes
            .iter()
            .flatten()
            .any(|envelope| envelope.param_id == id && envelope.enabled)
        || instance
            .audio_mods
            .iter()
            .flatten()
            .any(|audio| audio.param_id == id && audio.enabled)
        || instance
            .ableton_mappings
            .iter()
            .flatten()
            .any(|mapping| mapping.param_id == id)
        || instance
            .automation_lanes
            .iter()
            .flatten()
            .any(|lane| lane.param_id == id && lane.enabled)
}

fn nearly_equal(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-5_f32.max(1e-5 * a.abs().max(b.abs()))
}
