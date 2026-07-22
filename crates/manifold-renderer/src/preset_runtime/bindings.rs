//! String-typed outer-card bindings and D3 relight-knob writes for
//! [`PresetRuntime`] (generators only for strings). Extracted from
//! preset_runtime.rs (Wave 3 P3-R, design D3).

use super::*;

/// One resolved String outer-card → inner-node binding (generators only). The
/// String binding path stays bespoke (the shared float `apply` loop is
/// float-only): source is keyed by name (lookup into the host's
/// `clip.string_params` map), no convert because `String → String` is a
/// pass-through.
pub(super) struct StringBindingResolution {
    pub(super) target_node: NodeInstanceId,
    pub(super) target_param: String,
    /// Key into the host's `clip.string_params` map. The `presetMetadata`
    /// `stringBindings` `id` field — same identity as the matching
    /// `stringParams` entry's `id`.
    pub(super) source_key: String,
    pub(super) default: String,
    /// The def node's OWN value for the target param, captured from the
    /// (flattened) def at resolution time (BUG-182). Wins over `default`
    /// when seeding at construction, so a def-baked value — a file path set
    /// directly on the node, as the glb importer's mesh sources rely on —
    /// survives the build. `None` when the def leaves the param unset.
    pub(super) def_value: Option<String>,
}

/// Read a def-baked String param value for string-binding seeding: the
/// flattened def's node matching `node_id`, its literal `param` value if the
/// def sets one (BUG-182 — the def node param wins over the binding's
/// declared default at construction). Non-String serialized values can't
/// occur for a String-typed param (the loader type-checks), but a mismatch
/// degrades to `None` (= seed from the binding default) rather than failing
/// the build.
pub(super) fn def_string_param_value(
    flat_def: &manifold_core::effect_graph_def::EffectGraphDef,
    node_id: &manifold_core::NodeId,
    param: &str,
) -> Option<String> {
    let value = flat_def
        .nodes
        .iter()
        .find(|n| &n.node_id == node_id)?
        .params
        .get(param)?;
    match value {
        manifold_core::effect_graph_def::SerializedParamValue::String { value } => {
            Some(value.clone())
        }
        _ => None,
    }
}

/// One live D3 relight knob mapped to a runtime node param (unfused template
/// node) or a fused kernel uniform field. Built at chain-graph construction
/// and applied every frame so float-knob edits never need a structural
/// rebuild (`docs/DEPTH_RELIGHT_DESIGN.md` D8/P7).
#[derive(Clone, Debug)]
pub(super) struct RelightParamWrite {
    pub(super) field: RelightField,
    pub(super) target_inst: NodeInstanceId,
    pub(super) target_param: String,
    pub(super) scale: f32,
}

impl RelightParamWrite {
    pub(super) fn apply(&self, graph: &mut Graph, params: &RelightParams) {
        let value = self.field.get(params) * self.scale;
        let _ = graph.set_param(self.target_inst, &self.target_param, ParamValue::Float(value));
    }
}

/// Build the per-frame relight write list for one card/segment-member.
/// `fused_retarget` is the fused view's retarget map (single-card:
/// `LoadedPresetView::fused_retarget`; segment: `SegmentView::retarget`).
pub(super) fn build_relight_writes(
    relight: bool,
    handles: &[(std::borrow::Cow<'static, str>, NodeInstanceId)],
    node_map: &[(NodeId, NodeInstanceId)],
    fused_retarget: &AHashMap<(String, String), (NodeId, String)>,
    card_prefix: &str,
) -> Vec<RelightParamWrite> {
    if !relight {
        return Vec::new();
    }
    let mut writes = Vec::new();
    for field in RelightField::ALL {
        for target in crate::node_graph::relight::relight_field_targets(*field) {
            let handle = format!("{card_prefix}{}", target.node_handle);
            let param = target.param_name;
            if let Some((fused_node_id, fused_field)) =
                fused_retarget.get(&(handle.clone(), param.to_string()))
            {
                if let Some((_, inst)) = node_map.iter().find(|(nid, _)| nid == fused_node_id) {
                    writes.push(RelightParamWrite {
                        field: *field,
                        target_inst: *inst,
                        target_param: fused_field.clone(),
                        scale: target.scale,
                    });
                }
            } else if let Some((_, inst)) = handles.iter().find(|(h, _)| h == &handle) {
                writes.push(RelightParamWrite {
                    field: *field,
                    target_inst: *inst,
                    target_param: param.to_string(),
                    scale: target.scale,
                });
            }
        }
    }
    writes
}

impl PresetRuntime {
    /// Push the host's per-clip string overrides through the preset's
    /// `stringBindings` to the matching inner-node String params. Only keys
    /// PRESENT in `values` are written — an absent key leaves the live node
    /// param untouched (BUG-182: the previous fall-back-to-default behavior
    /// re-asserted the binding's declared default every frame, so a file path
    /// set directly on the node — e.g. `node.hdri_source`'s `path` via the
    /// graph editor's picker — was silently overwritten by the card's empty
    /// `hdri_file` default before the next frame ran). Defaults are seeded
    /// once at construction by [`Self::apply_string_defaults`], so absent
    /// keys still start from the binding default on a fresh runtime.
    pub fn apply_string_params(
        &mut self,
        values: Option<&std::collections::BTreeMap<String, String>>,
    ) {
        let Some(values) = values else { return };
        for binding in &self.string_bindings {
            let Some(v) = values.get(binding.source_key.as_str()) else {
                continue;
            };
            let _ = self.graph.set_param(
                binding.target_node,
                &binding.target_param,
                ParamValue::String(std::sync::Arc::new(v.clone())),
            );
        }
    }

    /// Seed every string binding's value once at construction, before the
    /// host's first `set_string_params` call. Precedence (BUG-182): the def
    /// node's OWN param value (`binding.def_value`, captured from the def at
    /// resolution time) wins over the binding's declared default, so a
    /// def-baked value — e.g. a file path set directly on the node in the
    /// graph editor — survives construction. Host values pushed later via
    /// [`Self::apply_string_params`] override either. (The live graph can't
    /// be consulted for this distinction: `Graph::add_node` pre-populates
    /// every declared param with its primitive default, so presence there
    /// says nothing about whether the DEF set the param.)
    pub(super) fn apply_string_defaults(&mut self) {
        for binding in &self.string_bindings {
            let seed = binding.def_value.as_ref().unwrap_or(&binding.default);
            let _ = self.graph.set_param(
                binding.target_node,
                &binding.target_param,
                ParamValue::String(std::sync::Arc::new(seed.clone())),
            );
        }
    }

    pub fn set_string_params(
        &mut self,
        params: Option<&std::collections::BTreeMap<String, String>>,
    ) {
        self.apply_string_params(params);
    }

    /// Push live "3D Shading" D3 relight knob values into the generator's
    /// spliced graph. Called by `GeneratorRenderer` every frame before
    /// `render` so float-knob edits never need a structural rebuild
    /// (`docs/DEPTH_RELIGHT_DESIGN.md` D8/P7).
    pub fn set_relight_params(&mut self, params: &RelightParams) {
        if let Some(slot) = self.effect_nodes.first() {
            slot.apply_relight_params(&mut self.graph, params);
        }
    }

}
