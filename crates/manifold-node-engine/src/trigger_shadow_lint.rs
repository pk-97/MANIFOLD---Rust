//! BUG-104 Part 5 — the shared trigger-shadow class-guard logic.
//!
//! One implementation, two consumers (Part 5's explicit requirement: "so
//! editor UI, MCP clients, and agent-authored graphs all see the same
//! warning" — not an editor-only lint):
//!
//! 1. `tests/trigger_shadow_class_guard.rs` — the workspace sweep over
//!    every bundled preset, gating CI.
//! 2. [`PresetRuntime::from_def`](crate::preset_runtime::PresetRuntime::from_def)
//!    — every generator (re)build, from ANY caller (editor rebuild-on-edit,
//!    an MCP tool call routed through `EditingService`/`MutateProject`, an
//!    agent-authored graph, `check-presets`, thumbnail render, freeze
//!    proofs) surfaces the same finding through
//!    [`crate::preset_runtime::ChainError`] / `PresetRuntime::errors()` —
//!    the existing structured-diagnostic channel (see that type's doc
//!    comment: "today this drives the consistent `[chain-error]` terminal
//!    log; tomorrow it's the data the editor reads"). No new channel: this
//!    is the smallest infra that already carries build-time warnings from
//!    a preset graph out to a caller, per `dont-cascade-redesign`.
//!
//! Effect-chain per-instance graph overrides are NOT wired to this check
//! yet — `PresetRuntime`'s effect-chain splice path builds from multiple
//! spliced effect instances rather than one preset's own `EffectGraphDef`
//! in isolation, and threading the check through that ~5000-line
//! state machine is out of scope for this pass. Generators (where BUG-104
//! itself lives — Lissajous is a generator) are fully covered. Flagged as
//! a scoped follow-up, not a silent gap.
//!
//! See `docs/BUG_BACKLOG.md` BUG-104 for the full investigation and the
//! Part 4 audit that produced [`DISCRETE_REPLACE_ALLOWLIST`].

use std::collections::HashMap;

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode};

/// `(preset_id, nodeId)` pairs where a trigger-driven `switch_value`
/// intentionally REPLACES rather than composes, per the BUG-104 Part 4
/// audit. Every entry's reasoning lives in the preset's own
/// `description` field (search for "BUG-104 audit") — this list and that
/// prose must stay in sync; if you add a node here, add the matching
/// sentence to the preset's description too (Part 4's "no silent
/// takeover" rule). Shared between the class-guard sweep test and the
/// live `PresetRuntime` build-time check so both agree on what's allowed.
pub const DISCRETE_REPLACE_ALLOWLIST: &[(&str, &str)] = &[
    ("BasicShapes", "gated_shape_idx"),
    ("BasicShapes", "gated_is_wireframe"),
    ("BasicShapes", "gated_target_angle"),
    ("BasicShapes", "target_angle_table"),
    ("ConcentricTunnel", "mux_final_n_sides"),
    ("ConcentricTunnel", "mux_cycle_sides"),
    ("Wireframe", "shape_mux"),
    ("MriVolume", "axis_mux"),
    ("Plasma", "pattern_mux"),
    ("StrangeAttractor", "type_mux"),
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
    ("Lissajous", "mux_x"),
    ("Lissajous", "mux_y"),
];

/// Control-rate node types whose `selector`/`in_N` input can carry a
/// trigger signal one hop further upstream. Candidate input port names
/// tried on each, matching each primitive's actual port surface.
const TRIGGER_PASSTHROUGH_TYPES: &[(&str, &[&str])] = &[
    ("node.math", &["a", "b"]),
    ("node.clip_trigger_cycle", &["trigger_count"]),
    ("node.clip_trigger_index", &["trigger_count"]),
    ("node.frequency_ratio", &["index"]),
    ("node.trigger_gate", &["trigger_count", "enable"]),
    ("node.sample_and_hold", &["trigger", "value"]),
    ("node.cycle_table_row", &["trigger_count"]),
    ("node.value", &["value"]),
];

/// Node types treated as simple continuous-value producers when tracing a
/// `switch_value` branch backward.
const CONTINUOUS_PASSTHROUGH_TYPES: &[&str] = &[
    "node.value",
    "node.lfo",
    "node.math",
    "node.smoothing",
    "node.envelope_follower_ar",
    "node.affine_scalar",
];

/// One finding: a trigger-driven `switch_value` node whose `in_N` branch
/// shadows a continuously-bound producer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerShadowFinding {
    /// Stable `nodeId` of the offending `node.switch_value` (or `#<id>`
    /// if it has none — shouldn't happen for a loaded, id-stamped doc).
    pub node_id: String,
    /// The shadowed input port (`in_0`..`in_7`).
    pub port: String,
    /// Human-readable description of the continuous binding being
    /// shadowed, for the diagnostic message.
    pub shadowed_source: String,
}

struct Ctx<'a> {
    nodes: HashMap<u32, &'a EffectGraphNode>,
    wires: HashMap<(u32, &'a str), (u32, &'a str)>,
    bindings: HashMap<(&'a str, &'a str), &'a str>,
    continuous_param: HashMap<&'a str, bool>,
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

    fn binding_is_continuous(&self, node_id: u32, port: &str) -> Option<bool> {
        let nid = self.node_id_str(node_id)?;
        let outer_id = self.bindings.get(&(nid, port))?;
        self.continuous_param.get(outer_id).copied()
    }

    fn is_trigger_source(&self, node_id: u32, port: &str, depth: u32) -> bool {
        if depth > 8 {
            return false;
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

/// Audit ONE preset's (already-flattened — pass the output of
/// `manifold_core::flatten::flatten_groups`) `EffectGraphDef` for the
/// BUG-104 class: a trigger-driven `switch_value` whose `in_N` branch
/// shadows a continuous outer binding. Does NOT consult
/// [`DISCRETE_REPLACE_ALLOWLIST`] — that's the caller's job (the sweep
/// test keys it by preset id; the live build-time check keys it the same
/// way via [`is_allowlisted`]).
pub fn find_trigger_shadow_findings(flat_def: &EffectGraphDef) -> Vec<TriggerShadowFinding> {
    let ctx = Ctx::build(flat_def);
    let mut findings = Vec::new();
    for node in &flat_def.nodes {
        if node.type_id != "node.switch_value" {
            continue;
        }
        if !ctx.is_trigger_source(node.id, "selector", 0) {
            continue;
        }
        let node_id = if node.node_id.is_empty() {
            format!("#{}", node.id)
        } else {
            node.node_id.as_str().to_string()
        };
        for i in 0..8 {
            let port = format!("in_{i}");
            if let Some(shadowed_source) = ctx.shadows_continuous_binding(node.id, &port, 0) {
                findings.push(TriggerShadowFinding {
                    node_id: node_id.clone(),
                    port,
                    shadowed_source,
                });
            }
        }
    }
    findings
}

/// Is `(preset_id, node_id)` in the documented BUG-104 Part 4 allowlist?
pub fn is_allowlisted(preset_id: &str, node_id: &str) -> bool {
    DISCRETE_REPLACE_ALLOWLIST
        .iter()
        .any(|(p, n)| *p == preset_id && *n == node_id)
}

/// Every `node.switch_value` node whose `selector` is trigger-driven,
/// REGARDLESS of whether it currently shadows a continuous binding — the
/// broader set [`find_trigger_shadow_findings`] narrows down to actual
/// violations. Used for allowlist hygiene tracking: most allowlisted
/// nodes (the Part 4 discrete-replace cases) are trigger-driven but
/// legitimately never shadow anything, so they never appear in
/// `find_trigger_shadow_findings`'s output — this is the set that lets a
/// caller confirm "yes, this node was actually checked," not just "no
/// violation was found for it."
pub fn find_trigger_driven_switch_value_node_ids(flat_def: &EffectGraphDef) -> Vec<String> {
    let ctx = Ctx::build(flat_def);
    flat_def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.switch_value")
        .filter(|n| ctx.is_trigger_source(n.id, "selector", 0))
        .map(|n| {
            if n.node_id.is_empty() {
                format!("#{}", n.id)
            } else {
                n.node_id.as_str().to_string()
            }
        })
        .collect()
}
