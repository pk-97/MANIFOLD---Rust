//! Per-modifier trigger event state for prepared scene-modifier graphs.
//!
//! Event streams are backed by ordinary `node.value` producers in the
//! prepared graph. The producers keep the graph dataflow explicit while this
//! cache keeps host clip/audio ownership out of the generated node IDs.

use ahash::{AHashMap, AHashSet};
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};

use crate::node_graph::parameters::ParamValue;
use crate::node_graph::{Graph, NodeInstanceId};

use super::SceneModifierExpandError;

/// Runtime targets for one modifier's generated event producers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneModifierEventRoute {
    pub modifier_id: NodeId,
    pub count_node: NodeId,
    pub baseline_node: NodeId,
}

#[derive(Debug, Clone)]
struct ModifierEvents {
    modifier_id: NodeId,
    count_node: NodeInstanceId,
    baseline_node: NodeInstanceId,
    gate_params: Vec<String>,
    count: u32,
    pending_baseline: Option<u32>,
}

/// Prepared event counters for the modifiers that consume trigger context.
#[derive(Debug, Clone)]
pub struct PreparedModifierEvents {
    modifiers: Vec<ModifierEvents>,
    by_modifier: AHashMap<NodeId, usize>,
    by_host_param: AHashMap<String, usize>,
    by_audio_key: AHashMap<u64, usize>,
}

impl PreparedModifierEvents {
    /// Resolve generated event producers and the host gate macros that own
    /// each modifier's audio stream.
    pub fn prepare(
        owner: &EffectGraphDef,
        routes: &[SceneModifierEventRoute],
        graph: &Graph,
    ) -> Result<Self, SceneModifierExpandError> {
        let mut modifier_ids = AHashSet::new();
        let mut by_modifier = AHashMap::with_capacity(routes.len());
        let mut used_nodes = AHashSet::with_capacity(routes.len() * 2);
        let mut modifiers = Vec::with_capacity(routes.len());

        for route in routes {
            if route.modifier_id.is_empty() || !modifier_ids.insert(route.modifier_id.clone()) {
                return Err(duplicate(
                    route.modifier_id.to_string(),
                    "event route modifier id is empty or duplicated",
                ));
            }
            if !owner
                .scene_modifiers
                .iter()
                .any(|modifier| modifier.id == route.modifier_id)
            {
                return Err(missing(
                    route.modifier_id.to_string(),
                    "event route references an unknown modifier",
                ));
            }
            if route.count_node == route.baseline_node
                || !used_nodes.insert(route.count_node.clone())
                || !used_nodes.insert(route.baseline_node.clone())
            {
                return Err(duplicate(
                    route.modifier_id.to_string(),
                    "event producers must be distinct across modifier routes",
                ));
            }
            let count_node = resolve_value_node(graph, &route.count_node, "count")?;
            let baseline_node = resolve_value_node(graph, &route.baseline_node, "baseline")?;
            let index = modifiers.len();
            by_modifier.insert(route.modifier_id.clone(), index);
            modifiers.push(ModifierEvents {
                modifier_id: route.modifier_id.clone(),
                count_node,
                baseline_node,
                gate_params: Vec::new(),
                count: 0,
                pending_baseline: None,
            });
        }

        let mut by_host_param = AHashMap::new();
        let mut param_keys = AHashMap::new();
        if let Some(metadata) = owner.preset_metadata.as_ref() {
            // Pulses reuse the existing allocation-free parameter hash. Check
            // all host parameter names so a token cannot alias another gate.
            for param in &metadata.params {
                let key = manifold_core::audio_trigger::fire_meter_key_for_param("", &param.id);
                if let Some(previous) = param_keys.insert(key, param.id.as_str())
                    && previous != param.id
                {
                    return Err(duplicate(
                        param.id.clone(),
                        "host parameter event token collision",
                    ));
                }
            }
            for binding in &metadata.bindings {
                let BindingTarget::SceneModifier {
                    modifier_id,
                    param_id,
                } = &binding.target
                else {
                    continue;
                };
                let modifier = owner
                    .scene_modifiers
                    .iter()
                    .find(|modifier| modifier.id == *modifier_id)
                    .ok_or_else(|| {
                        missing(
                            binding.id.clone(),
                            format!("binding targets unknown modifier {modifier_id}"),
                        )
                    })?;
                let is_gate = modifier
                    .graph
                    .preset_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.params.iter().find(|param| param.id == *param_id))
                    .map(|param| param.is_trigger_gate)
                    .ok_or_else(|| {
                        invalid(
                            binding.id.clone(),
                            format!("modifier parameter {param_id} is absent"),
                        )
                    })?;
                if !is_gate {
                    continue;
                }
                let Some(&modifier_index) = by_modifier.get(modifier_id) else {
                    // A modifier without a trigger context has no event route;
                    // its ordinary gate binding is not an event destination.
                    continue;
                };
                if binding.id.is_empty() {
                    return Err(invalid(
                        "presetMetadata.bindings",
                        "scene-modifier gate binding id is empty",
                    ));
                }
                if let Some(previous) = by_host_param.get(&binding.id) {
                    if *previous != modifier_index {
                        return Err(duplicate(
                            binding.id.clone(),
                            "one host gate macro cannot own multiple modifiers",
                        ));
                    }
                    continue;
                }
                by_host_param.insert(binding.id.clone(), modifier_index);
                modifiers[modifier_index]
                    .gate_params
                    .push(binding.id.clone());
            }
        }

        let by_audio_key = by_host_param
            .iter()
            .map(|(param, index)| {
                (
                    manifold_core::audio_trigger::fire_meter_key_for_param("", param),
                    *index,
                )
            })
            .collect();
        Ok(Self {
            modifiers,
            by_modifier,
            by_host_param,
            by_audio_key,
        })
    }

    /// Record one audio event for the modifier whose host gate macro fired.
    /// The first event before evaluation supplies the baseline for that owner.
    pub fn note_audio(&mut self, host_param: &str) -> bool {
        self.note_audio_key(manifold_core::audio_trigger::fire_meter_key_for_param(
            "", host_param,
        ))
    }

    pub fn note_audio_key(&mut self, param_key: u64) -> bool {
        let Some(&index) = self.by_audio_key.get(&param_key) else {
            return false;
        };
        bump(&mut self.modifiers[index]);
        true
    }

    /// Record one shared host clip edge for every modifier whose gate mode
    /// accepts clips. Modifiers without a gate macro accept the edge.
    pub fn note_clip<F>(&mut self, accepts: F) -> bool
    where
        F: Fn(&str) -> bool,
    {
        let mut changed = false;
        for modifier in &mut self.modifiers {
            if modifier.gate_params.is_empty()
                || modifier.gate_params.iter().any(|param| accepts(param))
            {
                bump(modifier);
                changed = true;
            }
        }
        changed
    }

    /// Write each modifier's current count and pending pre-event baseline to
    /// its prepared `node.value` producers.
    pub fn write_context(&self, graph: &mut Graph) {
        for modifier in &self.modifiers {
            graph.set_param_unchecked(
                modifier.count_node,
                "value",
                ParamValue::Float(modifier.count as f32),
            );
            let baseline = modifier.pending_baseline.unwrap_or(modifier.count);
            graph.set_param_unchecked(
                modifier.baseline_node,
                "value",
                ParamValue::Float(baseline as f32),
            );
        }
    }

    /// Clear event markers after the graph has consumed this frame's context.
    pub fn consume_pending(&mut self) {
        for modifier in &mut self.modifiers {
            modifier.pending_baseline = None;
        }
    }

    /// Reset all modifier event streams to a fresh-load state.
    pub fn clear(&mut self) {
        for modifier in &mut self.modifiers {
            modifier.count = 0;
            modifier.pending_baseline = None;
        }
    }

    /// Carry counters and pending markers by authored modifier identity while
    /// retaining this preparation's newly resolved runtime node IDs.
    pub fn carry_from(&mut self, prior: &Self) {
        for modifier in &mut self.modifiers {
            let Some(&previous_index) = prior.by_modifier.get(&modifier.modifier_id) else {
                continue;
            };
            let previous = &prior.modifiers[previous_index];
            modifier.count = previous.count;
            modifier.pending_baseline = previous.pending_baseline;
        }
    }

    /// Whether a host parameter is a scene-modifier gate macro.
    pub fn is_modifier_param(&self, param: &str) -> bool {
        self.by_host_param.contains_key(param)
    }
}

fn bump(modifier: &mut ModifierEvents) {
    if modifier.pending_baseline.is_none() {
        modifier.pending_baseline = Some(modifier.count);
    }
    modifier.count = modifier.count.wrapping_add(1);
}

fn resolve_value_node(
    graph: &Graph,
    node_id: &NodeId,
    role: &str,
) -> Result<NodeInstanceId, SceneModifierExpandError> {
    let runtime_id = graph
        .instance_by_node_id(node_id)
        .ok_or_else(|| missing(node_id.to_string(), format!("{role} event node is absent")))?;
    let instance = graph
        .get_node(runtime_id)
        .expect("instance_by_node_id returned an absent node");
    if instance.node.type_id().as_str() != "node.value" {
        return Err(invalid(
            node_id.to_string(),
            format!("{role} event node must be node.value"),
        ));
    }
    if !instance.params.contains_key("value") {
        return Err(invalid(
            node_id.to_string(),
            format!("{role} event node has no value parameter"),
        ));
    }
    Ok(runtime_id)
}

fn missing(path: impl Into<String>, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::MissingTarget {
        path: path.into(),
        detail: detail.into(),
    }
}

fn duplicate(path: impl Into<String>, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::DuplicateIdentity {
        path: path.into(),
        detail: detail.into(),
    }
}

fn invalid(path: impl Into<String>, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::InvalidBinding {
        path: path.into(),
        detail: detail.into(),
    }
}

#[cfg(test)]
mod scene_modifier_event_tests {
    use super::*;

    use crate::node_graph::persistence::{EffectGraphDefExt, PrimitiveRegistry};
    use manifold_core::effect_graph_def::EffectGraphDef;
    use serde_json::json;

    fn local_modifier(gate: bool) -> serde_json::Value {
        json!({
            "version": 1,
            "presetMetadata": {
                "id": "modifier",
                "displayName": "Modifier",
                "category": "Scene",
                "oscPrefix": "modifier",
                "params": [{
                    "id": "gate",
                    "name": "Gate",
                    "min": 0.0,
                    "max": 1.0,
                    "defaultValue": 0.0,
                    "isTriggerGate": gate
                }],
                "bindings": []
            },
            "nodes": [],
            "wires": []
        })
    }

    fn owner(modifiers: &[(&str, bool)], bindings: &[(&str, &str, &str)]) -> EffectGraphDef {
        let scene_modifiers: Vec<_> = modifiers
            .iter()
            .map(|(id, gate)| {
                json!({
                    "id": id,
                    "scene": {"node": "scene"},
                    "targets": "allObjects",
                    "graph": local_modifier(*gate)
                })
            })
            .collect();
        let host_bindings: Vec<_> = bindings
            .iter()
            .map(|(macro_id, modifier_id, param_id)| {
                json!({
                    "id": macro_id,
                    "label": macro_id,
                    "defaultValue": 0.0,
                    "target": {
                        "kind": "sceneModifier",
                        "modifierId": modifier_id,
                        "paramId": param_id
                    }
                })
            })
            .collect();
        serde_json::from_value(json!({
            "version": 3,
            "presetMetadata": {
                "id": "owner",
                "displayName": "Owner",
                "category": "Scene",
                "oscPrefix": "owner",
                "params": [],
                "bindings": host_bindings
            },
            "sceneModifiers": scene_modifiers,
            "nodes": [],
            "wires": []
        }))
        .expect("owner graph parses")
    }

    fn runtime_graph(node_ids: &[&str]) -> Graph {
        let nodes: Vec<_> = node_ids
            .iter()
            .enumerate()
            .map(|(index, node_id)| {
                json!({
                    "id": index,
                    "nodeId": node_id,
                    "typeId": "node.value",
                    "params": {"value": {"type": "Float", "value": 0.0}}
                })
            })
            .collect();
        let def: EffectGraphDef = serde_json::from_value(json!({
            "version": 1,
            "nodes": nodes,
            "wires": []
        }))
        .expect("runtime graph parses");
        def.into_graph(&PrimitiveRegistry::with_builtin())
            .expect("runtime graph builds")
    }

    fn route(modifier_id: &str, count_node: &str, baseline_node: &str) -> SceneModifierEventRoute {
        SceneModifierEventRoute {
            modifier_id: NodeId::new(modifier_id),
            count_node: NodeId::new(count_node),
            baseline_node: NodeId::new(baseline_node),
        }
    }

    fn value(graph: &Graph, node_id: &str) -> f32 {
        let runtime_id = graph
            .instance_by_node_id(&NodeId::new(node_id))
            .expect("value node runtime id");
        match graph
            .get_node(runtime_id)
            .expect("value node")
            .params
            .get("value")
        {
            Some(ParamValue::Float(value)) => *value,
            other => panic!("expected value parameter, got {other:?}"),
        }
    }

    #[test]
    fn audio_event_targets_only_its_modifier() {
        let owner = owner(
            &[("a", true), ("b", true)],
            &[("macro_a", "a", "gate"), ("macro_b", "b", "gate")],
        );
        let mut graph = runtime_graph(&["count_a", "baseline_a", "count_b", "baseline_b"]);
        let routes = vec![
            route("a", "count_a", "baseline_a"),
            route("b", "count_b", "baseline_b"),
        ];
        let mut events = PreparedModifierEvents::prepare(&owner, &routes, &graph).unwrap();
        assert!(events.note_audio("macro_a"));
        assert!(events.is_modifier_param("macro_a"));
        assert!(!events.is_modifier_param("ordinary"));
        events.write_context(&mut graph);
        assert_eq!(value(&graph, "count_a"), 1.0);
        assert_eq!(value(&graph, "baseline_a"), 0.0);
        assert_eq!(value(&graph, "count_b"), 0.0);
        assert_eq!(value(&graph, "baseline_b"), 0.0);
    }

    #[test]
    fn clip_event_reaches_both_and_respects_each_gate_mode() {
        let owner = owner(
            &[("a", true), ("b", true)],
            &[("macro_a", "a", "gate"), ("macro_b", "b", "gate")],
        );
        let mut graph = runtime_graph(&["count_a", "baseline_a", "count_b", "baseline_b"]);
        let routes = vec![
            route("a", "count_a", "baseline_a"),
            route("b", "count_b", "baseline_b"),
        ];
        let mut events = PreparedModifierEvents::prepare(&owner, &routes, &graph).unwrap();
        assert!(events.note_clip(|param| param == "macro_a" || param == "macro_b"));
        events.write_context(&mut graph);
        assert_eq!(value(&graph, "count_a"), 1.0);
        assert_eq!(value(&graph, "count_b"), 1.0);

        events.clear();
        assert!(events.note_clip(|param| param == "macro_a"));
        events.write_context(&mut graph);
        assert_eq!(value(&graph, "count_a"), 1.0);
        assert_eq!(value(&graph, "count_b"), 0.0);
    }

    #[test]
    fn modifier_without_gate_macro_accepts_clip_edges() {
        let owner = owner(&[("plain", false)], &[]);
        let mut graph = runtime_graph(&["count", "baseline"]);
        let mut events =
            PreparedModifierEvents::prepare(&owner, &[route("plain", "count", "baseline")], &graph)
                .unwrap();
        assert!(events.note_clip(|_| false));
        events.write_context(&mut graph);
        assert_eq!(value(&graph, "count"), 1.0);
        assert!(!events.note_audio("missing"));
    }

    #[test]
    fn simultaneous_events_keep_each_owner_earliest_baseline() {
        let owner = owner(
            &[("a", true), ("b", true)],
            &[("macro_a", "a", "gate"), ("macro_b", "b", "gate")],
        );
        let mut graph = runtime_graph(&["count_a", "baseline_a", "count_b", "baseline_b"]);
        let routes = vec![
            route("a", "count_a", "baseline_a"),
            route("b", "count_b", "baseline_b"),
        ];
        let mut events = PreparedModifierEvents::prepare(&owner, &routes, &graph).unwrap();
        assert!(events.note_audio("macro_a"));
        assert!(events.note_audio("macro_a"));
        assert!(events.note_audio("macro_b"));
        events.write_context(&mut graph);
        assert_eq!(value(&graph, "count_a"), 2.0);
        assert_eq!(value(&graph, "baseline_a"), 0.0);
        assert_eq!(value(&graph, "count_b"), 1.0);
        assert_eq!(value(&graph, "baseline_b"), 0.0);
        events.consume_pending();
        events.write_context(&mut graph);
        assert_eq!(value(&graph, "baseline_a"), 2.0);
        assert_eq!(value(&graph, "baseline_b"), 1.0);
    }

    #[test]
    fn carry_preserves_identity_across_reorder_removal_and_new_ids() {
        let prior_owner = owner(&[("a", true), ("b", true)], &[]);
        let current_owner = owner(&[("b", true), ("c", true)], &[]);
        let prior_routes = vec![
            route("a", "old_a_count", "old_a_baseline"),
            route("b", "old_b_count", "old_b_baseline"),
        ];
        let current_routes = vec![
            route("b", "new_b_count", "new_b_baseline"),
            route("c", "new_c_count", "new_c_baseline"),
        ];
        let mut prior_graph = runtime_graph(&[
            "old_a_count",
            "old_a_baseline",
            "old_b_count",
            "old_b_baseline",
        ]);
        let mut current_graph = runtime_graph(&[
            "new_b_count",
            "new_b_baseline",
            "new_c_count",
            "new_c_baseline",
        ]);
        let mut prior =
            PreparedModifierEvents::prepare(&prior_owner, &prior_routes, &prior_graph).unwrap();
        assert!(!prior.note_audio("missing"));
        assert!(prior.note_clip(|_| true));
        let mut current =
            PreparedModifierEvents::prepare(&current_owner, &current_routes, &current_graph)
                .unwrap();
        current.carry_from(&prior);
        current.write_context(&mut current_graph);
        assert_eq!(value(&current_graph, "new_b_count"), 1.0);
        assert_eq!(value(&current_graph, "new_c_count"), 0.0);
        prior.write_context(&mut prior_graph);
    }

    #[test]
    fn fresh_and_cleared_state_has_no_fake_event() {
        let owner = owner(&[("a", true)], &[]);
        let mut graph = runtime_graph(&["count", "baseline"]);
        let mut events =
            PreparedModifierEvents::prepare(&owner, &[route("a", "count", "baseline")], &graph)
                .unwrap();
        events.write_context(&mut graph);
        assert_eq!(value(&graph, "count"), 0.0);
        assert_eq!(value(&graph, "baseline"), 0.0);
        assert!(!events.note_audio("missing"));
        events.clear();
        events.write_context(&mut graph);
        assert_eq!(value(&graph, "count"), 0.0);
        assert_eq!(value(&graph, "baseline"), 0.0);
    }

    #[test]
    fn preparation_rejects_unknown_and_duplicate_routes() {
        let owner = owner(&[("a", true)], &[]);
        let graph = runtime_graph(&["count", "baseline"]);
        assert!(matches!(
            PreparedModifierEvents::prepare(
                &owner,
                &[route("missing", "count", "baseline")],
                &graph,
            ),
            Err(SceneModifierExpandError::MissingTarget { .. })
        ));
        assert!(matches!(
            PreparedModifierEvents::prepare(
                &owner,
                &[
                    route("a", "count", "baseline"),
                    route("a", "count", "baseline"),
                ],
                &graph,
            ),
            Err(SceneModifierExpandError::DuplicateIdentity { .. })
        ));

        let conflicting_owner = self::owner(
            &[("a", true), ("b", true)],
            &[("shared", "a", "gate"), ("shared", "b", "gate")],
        );
        let conflict_graph = runtime_graph(&["count_a", "baseline_a", "count_b", "baseline_b"]);
        assert!(matches!(
            PreparedModifierEvents::prepare(
                &conflicting_owner,
                &[
                    route("a", "count_a", "baseline_a"),
                    route("b", "count_b", "baseline_b"),
                ],
                &conflict_graph,
            ),
            Err(SceneModifierExpandError::DuplicateIdentity { .. })
        ));
    }
}
