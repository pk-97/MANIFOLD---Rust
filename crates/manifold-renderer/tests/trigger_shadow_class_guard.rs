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
//! This guard checks both paths, and — via `manifold_core::flatten::
//! flatten_groups` — resolves wires transparently across group boundaries
//! (Lissajous's `mux_x`/`mux_y` live inside the "Frequency Selection"
//! group; flattening removes the group_input/group_output hop entirely so
//! the trace never needs to hand-walk scopes).
//!
//! Discrete/toggle/whole-number targets are the documented exception (BUG-104
//! Part 4 walked every trigger-driven `switch_value` in the shipped library
//! and recorded, in each preset's own `description`, why "replace" is the
//! correct behavior there — cycling a shape/axis/pattern/attractor-type/
//! simulation-mode enum on trigger is legible, not a shadowed fader). Those
//! nodes are allowlisted below by `(preset_id, nodeId)`, each entry citing
//! the preset's own description as the record of the decision.
//!
//! Run: `cargo nextest run -p manifold-renderer --test trigger_shadow_class_guard`
//! (GPU-free — pure static analysis over `EffectGraphDef`, safe for the
//! default sweep).

use std::collections::{HashMap, HashSet};

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode};
use manifold_core::flatten::flatten_groups;
use manifold_renderer::preset_loader::{EFFECT_CATALOG, GENERATOR_CATALOG};

/// `(preset_id, nodeId)` pairs where a trigger-driven `switch_value`
/// intentionally REPLACES rather than composes, per the BUG-104 Part 4
/// audit. Every entry's reasoning lives in the preset's own
/// `description` field (search for "BUG-104 audit") — this list and
/// that prose must stay in sync; if you add a node here, add the
/// matching sentence to the preset's description too (Part 4's "no
/// silent takeover" rule).
const DISCRETE_REPLACE_ALLOWLIST: &[(&str, &str)] = &[
    // BasicShapes — discrete shape / fill-mode / target-angle options.
    ("BasicShapes", "gated_shape_idx"),
    ("BasicShapes", "gated_is_wireframe"),
    ("BasicShapes", "gated_target_angle"),
    ("BasicShapes", "target_angle_table"),
    // ConcentricTunnel — discrete side-count options.
    ("ConcentricTunnel", "mux_final_n_sides"),
    ("ConcentricTunnel", "mux_cycle_sides"),
    // Wireframe — discrete shape enum.
    ("Wireframe", "shape_mux"),
    // MriVolume — discrete scan-axis enum.
    ("MriVolume", "axis_mux"),
    // Plasma — discrete pattern-algorithm enum.
    ("Plasma", "pattern_mux"),
    // StrangeAttractor — discrete attractor-type enum.
    ("StrangeAttractor", "type_mux"),
    // FluidSim2D / FluidSim3D / ParticleText — discrete simulation-mode
    // index selectors (mode_hold-driven, or boot_gate-driven for
    // FluidSim3D's boot_pattern_mux). FluidSim3D's shape is the
    // reference case the original BUG-104 investigation used to confirm
    // "discrete mode selection on trigger" is legitimate.
    ("FluidSim2D", "mux_turbulence"),
    ("FluidSim2D", "mux_rotation"),
    ("FluidSim2D", "mux_slope"),
    ("FluidSim2D", "mux_mode3"),
    ("FluidSim2D", "mux_mode4"),
    ("FluidSim3D", "mux_noise"),
    ("FluidSim3D", "mux_curl"),
    ("FluidSim3D", "mux_flow"),
    ("FluidSim3D", "mux_mode3"),
    ("FluidSim3D", "mux_mode4"),
    ("FluidSim3D", "boot_pattern_mux"),
    ("ParticleText", "mux_mode3"),
    ("ParticleText", "mux_mode4"),
    ("ParticleText", "mux_turbulence"),
    ("ParticleText", "mux_rotation"),
    ("ParticleText", "mux_slope"),
    // Lissajous — mux_x/mux_y's SELECTOR is still trigger-driven after the
    // BUG-104 Part 3 fix (that's unchanged and correct — the trigger still
    // needs to pick between identity and the harmonic ratio). What changed
    // is that neither branch is wired to a continuously-bound node anymore
    // (in_0 is now a static `1.0` identity constant, not `freqX`/`freqY`),
    // so this guard should not actually trip on them — listed anyway as an
    // explicit, auditable record that they were checked, not missed.
    ("Lissajous", "mux_x"),
    ("Lissajous", "mux_y"),
];

/// Control-rate node types whose `selector`/`in_N` input can carry a
/// trigger signal one hop further upstream — the "VIA" hop in the trace.
/// Candidate input port names tried on each, matching each primitive's
/// actual port surface (see the primitive source under
/// `crates/manifold-renderer/src/node_graph/primitives/`).
const TRIGGER_PASSTHROUGH_TYPES: &[(&str, &[&str])] = &[
    ("node.math", &["a", "b"]),
    ("node.clip_trigger_cycle", &["trigger_count"]),
    ("node.clip_trigger_index", &["trigger_count"]),
    ("node.frequency_ratio", &["index"]),
    ("node.trigger_gate", &["trigger_count", "enable"]),
    ("node.sample_and_hold", &["trigger", "value"]),
    ("node.cycle_table_row", &["trigger_count"]),
    // A `node.value` constant is itself a valid trigger-carrying hop when
    // its OWN `value` param is written by a binding to a trigger-flagged
    // outer param (Wireframe's `shape_mux.selector` <- `clip_trigger_value`
    // (node.value) <- outer `clip_trigger` (isTriggerGate)). The direct-
    // binding check inside `is_trigger_source` handles this once we let
    // the trace step onto the value node's own `value` port.
    ("node.value", &["value"]),
];

/// Node types treated as simple continuous-value producers when tracing a
/// `switch_value` branch backward — pass through to check whether THIS
/// node (not just the switch's own port) is the target of a continuous
/// outer-card binding.
const CONTINUOUS_PASSTHROUGH_TYPES: &[&str] = &[
    "node.value",
    "node.lfo",
    "node.math",
    "node.smoothing",
    "node.envelope_follower_ar",
    "node.affine_scalar",
];

struct Ctx<'a> {
    nodes: HashMap<u32, &'a EffectGraphNode>,
    /// (to_node, to_port) -> (from node id, from port name)
    wires: HashMap<(u32, &'a str), (u32, &'a str)>,
    /// (target nodeId string, target param name) -> outer binding id
    bindings: HashMap<(&'a str, &'a str), &'a str>,
    /// outer param id -> is this a CONTINUOUS param (not toggle, not
    /// trigger, not trigger-gate, not whole-number)?
    continuous_param: HashMap<&'a str, bool>,
    /// outer param id -> is this param explicitly trigger-flagged
    /// (`isTrigger` or `isTriggerGate`)?
    trigger_param: HashMap<&'a str, bool>,
}

impl<'a> Ctx<'a> {
    fn build(def: &'a EffectGraphDef) -> Self {
        let nodes: HashMap<u32, &EffectGraphNode> = def.nodes.iter().map(|n| (n.id, n)).collect();
        let mut wires: HashMap<(u32, &str), (u32, &str)> = HashMap::new();
        for w in &def.wires {
            wires
                .entry((w.to_node, w.to_port.as_str()))
                .or_insert((w.from_node, w.from_port.as_str()));
        }
        let mut bindings: HashMap<(&str, &str), &str> = HashMap::new();
        let mut continuous_param: HashMap<&str, bool> = HashMap::new();
        let mut trigger_param: HashMap<&str, bool> = HashMap::new();
        if let Some(meta) = &def.preset_metadata {
            for p in &meta.params {
                let is_continuous =
                    !p.is_toggle && !p.is_trigger && !p.is_trigger_gate && !p.whole_numbers;
                continuous_param.insert(p.id.as_str(), is_continuous);
                trigger_param.insert(p.id.as_str(), p.is_trigger || p.is_trigger_gate);
            }
            for b in &meta.bindings {
                if let BindingTarget::Node { node_id, param } = &b.target {
                    bindings.insert((node_id.as_str(), param.as_str()), b.id.as_str());
                }
            }
        }
        Self { nodes, wires, bindings, continuous_param, trigger_param }
    }

    fn node_id_str(&self, id: u32) -> Option<&'a str> {
        self.nodes
            .get(&id)
            .map(|n| n.node_id.as_str())
            .filter(|s| !s.is_empty())
    }

    /// Does a binding target `(node_id, port)`, and if so, is the outer
    /// param it routes CONTINUOUS?
    fn binding_is_continuous(&self, node_id: u32, port: &str) -> Option<bool> {
        let nid = self.node_id_str(node_id)?;
        let outer_id = self.bindings.get(&(nid, port))?;
        self.continuous_param.get(outer_id).copied()
    }

    /// Trace whether `(node_id, port)` derives from a trigger source —
    /// either `system.generator_input`'s trigger output, or an outer
    /// binding flagged `isTrigger`/`isTriggerGate` writing straight into
    /// this exact port, or (recursively) a control-rate node whose own
    /// input is a trigger source.
    fn is_trigger_source(&self, node_id: u32, port: &str, depth: u32) -> bool {
        if depth > 8 {
            return false; // defensive cycle guard; these graphs are DAGs
        }
        if let Some(nid) = self.node_id_str(node_id)
            && let Some(outer_id) = self.bindings.get(&(nid, port))
            && self.trigger_param.get(outer_id).copied().unwrap_or(false)
        {
            return true;
        }
        let Some((from_node, from_port)) = self.wires.get(&(node_id, port)).copied() else {
            return false;
        };
        let Some(fn_node) = self.nodes.get(&from_node) else { return false };
        let type_id = fn_node.type_id.as_str();
        if type_id == "system.generator_input" || type_id == "system.source" {
            return from_port == "trigger_count"
                || from_port == "trigger"
                || from_port.starts_with("trigger");
        }
        for (t, ports) in TRIGGER_PASSTHROUGH_TYPES {
            if *t == type_id {
                return ports
                    .iter()
                    .any(|p| self.is_trigger_source(from_node, p, depth + 1));
            }
        }
        false
    }

    /// Does `(node_id, port)`, tracing backward through wires (and a
    /// bounded chain of continuous-passthrough nodes), reach a node that a
    /// CONTINUOUS outer binding targets — either on the exact port itself,
    /// or on ANY param of the immediate producer node (the Lissajous
    /// pre-fix shape: `lfo_x.out` fed `mux_x.in_0`, and it was
    /// `lfo_x.angular_rate` — a different param on the SAME node — that
    /// carried the continuous binding)?
    fn shadows_continuous_binding(&self, node_id: u32, port: &str, depth: u32) -> Option<String> {
        if depth > 6 {
            return None;
        }
        if let Some(true) = self.binding_is_continuous(node_id, port) {
            let nid = self.node_id_str(node_id).unwrap_or("?");
            return Some(format!("{nid}.{port}"));
        }
        let (from_node, from_port) = self.wires.get(&(node_id, port)).copied()?;
        let fn_node = self.nodes.get(&from_node)?;
        let type_id = fn_node.type_id.as_str();

        if let Some(nid) = self.node_id_str(from_node) {
            for ((bnid, bparam), outer_id) in &self.bindings {
                if *bnid == nid && self.continuous_param.get(outer_id).copied().unwrap_or(false) {
                    return Some(format!("{nid}.{bparam} (feeds {port} via {type_id})"));
                }
            }
        }

        if CONTINUOUS_PASSTHROUGH_TYPES.contains(&type_id) {
            for p in ["a", "b", "value", "in"] {
                if let Some(found) = self.shadows_continuous_binding(from_node, p, depth + 1) {
                    return Some(found);
                }
            }
        }
        let _ = from_port;
        None
    }
}

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
    let ctx = Ctx::build(&flat);

    for node in &flat.nodes {
        if node.type_id != "node.switch_value" {
            continue;
        }
        if !ctx.is_trigger_source(node.id, "selector", 0) {
            continue;
        }
        let node_id_str = if node.node_id.is_empty() {
            format!("#{}", node.id)
        } else {
            node.node_id.as_str().to_string()
        };
        let allowed = DISCRETE_REPLACE_ALLOWLIST
            .iter()
            .any(|(p, n)| *p == preset_id && *n == node_id_str);
        if allowed {
            allowlist_hits.insert((preset_id.to_string(), node_id_str.clone()));
        }
        for i in 0..8 {
            let port = format!("in_{i}");
            if let Some(source) = ctx.shadows_continuous_binding(node.id, &port, 0)
                && !allowed
            {
                violations.push(format!(
                    "preset `{preset_id}` node `{node_id_str}`.{port} (trigger-driven selector) \
                     shadows a continuous binding at {source}. If this is an intentional discrete \
                     replace, add (\"{preset_id}\", \"{node_id_str}\") to DISCRETE_REPLACE_ALLOWLIST \
                     AND record the decision in the preset's `description` (BUG-104 Part 4 convention)."
                ));
            }
        }
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
    // either the preset was renamed/removed, or (as documented above for
    // Lissajous) the fix already made the node structurally safe and the
    // entry is a deliberate audit record rather than a live exception.
    for (preset_id, node_id) in DISCRETE_REPLACE_ALLOWLIST {
        if !allowlist_hits.contains(&(preset_id.to_string(), node_id.to_string())) {
            eprintln!(
                "note: allowlist entry (\"{preset_id}\", \"{node_id}\") did not match any \
                 trigger-driven switch_value node this run — expected for Lissajous's mux_x/mux_y \
                 post-BUG-104-Part-3 (see the allowlist comment); verify for anything else."
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
