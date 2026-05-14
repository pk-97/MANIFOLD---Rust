//! Per-instance effect graph schema — the on-disk shape an
//! [`EffectInstance`](crate::effects::EffectInstance) carries when its
//! graph topology has diverged from the catalog default.
//!
//! These types are pure serde shapes: zero references back into the
//! live runtime graph, zero GPU types. They live in `manifold-core`
//! so [`EffectInstance`](crate::effects::EffectInstance) can hold one
//! by value without dragging `manifold-renderer` into the dependency
//! graph.
//!
//! The renderer round-trips between [`EffectGraphDef`] and its live
//! `Graph` via `manifold_renderer::node_graph::persistence` — that's
//! where the [`PrimitiveRegistry`] and the `ParamValue` ↔
//! [`SerializedParamValue`] conversions live.
//!
//! ## Versioning
//!
//! [`EffectGraphDef::version`] starts at `1`. A document with a higher
//! version fails to load — old binaries don't know how to read future
//! graphs. Bump the constant and add a migration when the schema
//! changes incompatibly.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Current schema version emitted by new saves. Documents with a higher
/// version fail to load on this binary.
pub const EFFECT_GRAPH_VERSION: u32 = 1;

/// Top-level shape for one effect's per-instance graph.
///
/// Same schema used by bundled preset libraries
/// (`assets/effect-presets/*.json`) and by per-instance graph
/// overrides stored on an [`EffectInstance`](crate::effects::EffectInstance).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectGraphDef {
    pub version: u32,
    /// Display name for the preset library / saved graph picker.
    /// `None` when the graph is anonymous (e.g., a per-instance override
    /// that wasn't promoted to a named preset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Free-form description shown in the picker tooltip. `None` when
    /// the author didn't supply one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub nodes: Vec<EffectGraphNode>,
    pub wires: Vec<EffectGraphWire>,
}

/// One node inside an [`EffectGraphDef`]. `id` is unique within the
/// document and is the wire-endpoint key — it survives load by mapping
/// to a fresh runtime `NodeInstanceId`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectGraphNode {
    pub id: u32,
    pub type_id: String,
    /// Stable string handle to pass to `Graph::add_node_named` so
    /// user-exposed parameter bindings can address this inner node
    /// across renderer refactors. `None` for anonymous nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Per-parameter overrides keyed by stable param name. Missing
    /// keys fall through to the node's declared defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, SerializedParamValue>,
    /// Editor-saved position in graph-space. `None` for documents
    /// authored without an editor (hand-rolled bundled presets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_pos: Option<(f32, f32)>,
}

/// One wire inside an [`EffectGraphDef`]. Endpoint ids reference
/// [`EffectGraphNode::id`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectGraphWire {
    pub from_node: u32,
    pub from_port: String,
    pub to_node: u32,
    pub to_port: String,
}

/// Tagged-enum wire form of the renderer's `ParamValue`. Tagged because
/// untagged would conflate `Float(0.0)` / `Int(0)` / `Bool(false)`.
///
/// Conversions to/from the renderer's `ParamValue` live in
/// `manifold_renderer::node_graph::persistence`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum SerializedParamValue {
    Float { value: f32 },
    Int { value: i32 },
    Bool { value: bool },
    Vec2 { value: [f32; 2] },
    Vec3 { value: [f32; 3] },
    Vec4 { value: [f32; 4] },
    Color { value: [f32; 4] },
    Enum { value: u32 },
}

impl EffectGraphDef {
    /// Set the display name. Builder-style convenience for bundled
    /// preset constructors.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the description. Builder-style.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_graph_round_trips() {
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        };
        let json = serde_json::to_string(&def).unwrap();
        let back: EffectGraphDef = serde_json::from_str(&json).unwrap();
        assert_eq!(def, back);
    }

    #[test]
    fn serialized_param_value_round_trips_every_variant() {
        let cases = [
            SerializedParamValue::Float { value: 0.5 },
            SerializedParamValue::Int { value: 7 },
            SerializedParamValue::Bool { value: true },
            SerializedParamValue::Vec2 { value: [1.0, 2.0] },
            SerializedParamValue::Vec3 {
                value: [1.0, 2.0, 3.0],
            },
            SerializedParamValue::Vec4 {
                value: [1.0, 2.0, 3.0, 4.0],
            },
            SerializedParamValue::Color {
                value: [0.1, 0.2, 0.3, 1.0],
            },
            SerializedParamValue::Enum { value: 3 },
        ];
        for v in cases {
            let json = serde_json::to_string(&v).unwrap();
            let back: SerializedParamValue = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn name_and_description_skipped_when_none() {
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        };
        let json = serde_json::to_string(&def).unwrap();
        assert!(!json.contains("\"name\""));
        assert!(!json.contains("\"description\""));
    }

    #[test]
    fn node_with_param_overrides_round_trips() {
        let mut params = BTreeMap::new();
        params.insert(
            "level".to_string(),
            SerializedParamValue::Float { value: 0.8 },
        );
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: Some("Custom Threshold".to_string()),
            description: None,
            nodes: vec![EffectGraphNode {
                id: 0,
                type_id: "node.threshold".to_string(),
                handle: Some("thresh".to_string()),
                params,
                editor_pos: Some((100.0, 200.0)),
            }],
            wires: Vec::new(),
        };
        let json = serde_json::to_string(&def).unwrap();
        let back: EffectGraphDef = serde_json::from_str(&json).unwrap();
        assert_eq!(def, back);
    }
}
