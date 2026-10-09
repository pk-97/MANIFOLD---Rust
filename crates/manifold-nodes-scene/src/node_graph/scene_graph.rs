//! Shared scene graph constructors for import and native scene authoring.
use std::collections::{BTreeMap, BTreeSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphNode, EffectGraphWire, SerializedParamValue};

/// Build an [`EffectGraphNode`] with the given identity and every other
/// field at its "ordinary node" default. `EffectGraphNode` doesn't derive
/// `Default` (several fields are meaningful `Option`s used by grouping /
/// the graph editor), so this centralises the shape once rather than
/// repeating all eleven fields at every call site.
pub(crate) fn plain_node(id: u32, node_id: &str, type_id: &str, handle: &str) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: NodeId::new(node_id),
        type_id: type_id.to_string(),
        handle: Some(handle.to_string()),
        params: BTreeMap::new(),
        exposed_params: BTreeSet::new(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

manifold_core::testkit_visible! {
pub(crate) fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    }
}
}

manifold_core::testkit_visible! {
pub(crate) fn float(v: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value: v }
}
}
manifold_core::testkit_visible! {
pub(crate) fn int(v: i32) -> SerializedParamValue {
    SerializedParamValue::Int { value: v }
}
}
manifold_core::testkit_visible! {
pub(crate) fn bool_val(v: bool) -> SerializedParamValue {
    SerializedParamValue::Bool { value: v }
}
}
manifold_core::testkit_visible! {
pub(crate) fn enum_val(v: u32) -> SerializedParamValue {
    SerializedParamValue::Enum { value: v }
}
}
pub(crate) fn table(rows: Vec<Vec<f32>>) -> SerializedParamValue {
    SerializedParamValue::Table { rows }
}

