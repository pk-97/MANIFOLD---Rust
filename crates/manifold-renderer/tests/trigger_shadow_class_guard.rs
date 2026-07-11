//! BUG-104 Part 5(a) — class guard: no `node.switch_value` whose `selector`
//! transitively derives from a trigger source (`system.generator_input`'s
//! `trigger_count`/`trigger` output, or an outer-card param flagged
//! `isTrigger`/`isTriggerGate`) may shadow a CONTINUOUS user binding on one
//! of its `in_0..in_7` branches — that is exactly the mechanism BUG-104
//! found on Lissajous's `mux_x`/`mux_y`: firing the trigger replaced the
//! continuous-fader branch outright instead of composing onto it, so the
//! fader went dead while the trigger was active.
//!
//! "Transitively" matters: the original ad-hoc scan that first surfaced
//! this bug class only checked DIRECT wires into `selector` and produced a
//! false negative on Lissajous, whose `mux_x.selector` / `mux_y.selector`
//! are driven by a `presetMetadata.bindings` entry (an outer-card param
//! written straight into the node's param every frame), not a graph wire.
//! The shared checker (`manifold_renderer::node_graph::trigger_shadow_lint`)
//! checks both paths, and — via `manifold_core::flatten::flatten_groups` —
//! resolves wires transparently across group boundaries (Lissajous's
//! `mux_x`/`mux_y` live inside the "Frequency Selection" group; flattening
//! removes the group_input/group_output hop entirely so the trace never
//! needs to hand-walk scopes).
//!
//! This is ONE of two consumers of that shared checker — the other is
//! `PresetRuntime::from_def` (every live generator build), so editor UI,
//! MCP-driven mutations, and agent-authored graphs all see the same
//! finding via `PresetRuntime::errors()`, not just this offline sweep. See
//! `trigger_shadow_lint`'s module doc for the full Part 5(b) design.
//!
//! Discrete/toggle/whole-number targets are the documented exception (BUG-104
//! Part 4 walked every trigger-driven `switch_value` in the shipped library
//! and recorded, in each preset's own `description`, why "replace" is the
//! correct behavior there). Those nodes are allowlisted in
//! `trigger_shadow_lint::DISCRETE_REPLACE_ALLOWLIST`, each entry citing the
//! preset's own description as the record of the decision.
//!
//! Run: `cargo nextest run -p manifold-renderer --test trigger_shadow_class_guard`
//! (GPU-free — pure static analysis over `EffectGraphDef`, safe for the
//! default sweep).

use std::collections::HashSet;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::flatten::flatten_groups;
use manifold_renderer::node_graph::trigger_shadow_lint::{
    DISCRETE_REPLACE_ALLOWLIST, find_trigger_driven_switch_value_node_ids,
    find_trigger_shadow_findings, is_allowlisted,
};
use manifold_renderer::preset_loader::{EFFECT_CATALOG, GENERATOR_CATALOG};

fn audit_def(
    preset_id: &str,
    def: &EffectGraphDef,
    violations: &mut Vec<String>,
    allowlist_hits: &mut HashSet<(String, String)>,
) {
    let flat = match flatten_groups(def) {
        Ok(f) => f,
        Err(e) => panic!("preset `{preset_id}`: flatten_groups failed: {e:?}"),
    };
    // Every trigger-driven switch_value counts as "checked" for hygiene
    // purposes, whether or not it ends up shadowing anything — most
    // allowlisted (Part 4 discrete-replace) nodes never shadow, and that's
    // exactly the point.
    for node_id in find_trigger_driven_switch_value_node_ids(&flat) {
        if is_allowlisted(preset_id, &node_id) {
            allowlist_hits.insert((preset_id.to_string(), node_id));
        }
    }
    for finding in find_trigger_shadow_findings(&flat) {
        let allowed = is_allowlisted(preset_id, &finding.node_id);
        if allowed {
            continue;
        }
        violations.push(format!(
            "preset `{preset_id}` node `{}`.{} (trigger-driven selector) shadows a continuous \
             binding at {}. If this is an intentional discrete replace, add (\"{preset_id}\", \
             \"{}\") to trigger_shadow_lint::DISCRETE_REPLACE_ALLOWLIST AND record the decision \
             in the preset's `description` (BUG-104 Part 4 convention).",
            finding.node_id, finding.port, finding.shadowed_source, finding.node_id
        ));
    }
}

fn run_catalog_audit(
    entries: impl Iterator<Item = (std::sync::Arc<str>, std::sync::Arc<str>)>,
) -> (Vec<String>, HashSet<(String, String)>) {
    let mut violations = Vec::new();
    let mut allowlist_hits = HashSet::new();
    for (preset_id, json) in entries {
        let def: EffectGraphDef = match serde_json::from_str(&json) {
            Ok(d) => d,
            Err(_) => continue, // legacy/malformed docs are covered by other sweep tests
        };
        audit_def(&preset_id, &def, &mut violations, &mut allowlist_hits);
    }
    (violations, allowlist_hits)
}

#[test]
fn no_trigger_driven_switch_value_shadows_a_continuous_binding_outside_the_allowlist() {
    let (mut violations, mut allowlist_hits) = run_catalog_audit(GENERATOR_CATALOG.load().entries());
    let (eff_violations, eff_hits) = run_catalog_audit(EFFECT_CATALOG.load().entries());
    violations.extend(eff_violations);
    allowlist_hits.extend(eff_hits);

    assert!(
        violations.is_empty(),
        "BUG-104 class guard failed — trigger-driven switch_value node(s) shadow a continuous \
         binding without being in the documented allowlist:\n  - {}",
        violations.join("\n  - ")
    );

    // Soft hygiene check (prints, does not fail): allowlist entries this
    // sweep never actually matched a trigger-driven switch_value node —
    // either the preset was renamed/removed, or (as documented in
    // trigger_shadow_lint for Lissajous) the fix already made the node
    // structurally safe and the entry is a deliberate audit record rather
    // than a live exception.
    for (preset_id, node_id) in DISCRETE_REPLACE_ALLOWLIST {
        if !allowlist_hits.contains(&(preset_id.to_string(), node_id.to_string())) {
            eprintln!(
                "note: allowlist entry (\"{preset_id}\", \"{node_id}\") did not match any \
                 trigger-driven switch_value node this run — expected for Lissajous's mux_x/mux_y \
                 post-BUG-104-Part-3 (see trigger_shadow_lint's allowlist comment); verify for \
                 anything else."
            );
        }
    }
}

/// Regression proof the guard has teeth: a synthetic minimal doc
/// reproducing the EXACT pre-fix Lissajous shape (a `switch_value` whose
/// `selector` is bound to a trigger-flagged outer param, and whose `in_0`
/// is wired to an `lfo` node whose `angular_rate` param is bound to a
/// CONTINUOUS outer param) must be flagged when the node isn't allowlisted.
/// If this test ever goes green with `violations` empty, the guard has
/// silently lost its teeth.
#[test]
fn synthetic_pre_fix_lissajous_shape_is_flagged() {
    let json = r#"{
        "version": 2,
        "name": "SyntheticPreFixShape",
        "nodes": [
            { "id": 0, "nodeId": "input", "typeId": "system.generator_input" },
            { "id": 1, "nodeId": "lfo_x", "typeId": "node.lfo",
              "params": { "angular_rate": { "type": "Float", "value": 0.13 } } },
            { "id": 2, "nodeId": "mux_x", "typeId": "node.switch_value" },
            { "id": 3, "nodeId": "final_output", "typeId": "system.final_output" }
        ],
        "wires": [
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in_0" }
        ],
        "presetMetadata": {
            "id": "SyntheticPreFixShape",
            "displayName": "Synthetic",
            "category": "Geometry",
            "oscPrefix": "synthetic",
            "params": [
                { "id": "freq_x_rate", "name": "Freq X Rate", "min": 0.0, "max": 1.0,
                  "defaultValue": 0.13, "wholeNumbers": false, "isToggle": false, "isTrigger": false },
                { "id": "clip_trigger", "name": "Clip Trigger", "min": 0.0, "max": 1.0,
                  "defaultValue": 0.0, "wholeNumbers": false, "isToggle": true, "isTriggerGate": true, "isTrigger": false }
            ],
            "bindings": [
                { "id": "freq_x_rate", "label": "Freq X Rate", "defaultValue": 0.13,
                  "target": { "kind": "node", "nodeId": "lfo_x", "param": "angular_rate" },
                  "convert": { "type": "Float" } },
                { "id": "clip_trigger", "label": "Clip Trigger", "defaultValue": 0.0,
                  "target": { "kind": "node", "nodeId": "mux_x", "param": "selector" },
                  "convert": { "type": "Float" } }
            ]
        }
    }"#;
    let def: EffectGraphDef = serde_json::from_str(json).expect("synthetic doc must parse");
    let mut violations = Vec::new();
    let mut allowlist_hits = HashSet::new();
    audit_def("SyntheticPreFixShape", &def, &mut violations, &mut allowlist_hits);
    assert_eq!(
        violations.len(),
        1,
        "the pre-fix shape (continuous binding on lfo_x.angular_rate, wired into a \
         trigger-selected mux_x.in_0) must be flagged exactly once — got {violations:?}"
    );
    assert!(allowlist_hits.is_empty(), "this synthetic node is not in the allowlist");
}
