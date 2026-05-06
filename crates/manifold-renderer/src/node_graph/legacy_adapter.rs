//! [`LegacyPostProcessNode`] — adapter that wraps any
//! [`PostProcessEffect`] as a single [`EffectNode`].
//!
//! Bridges the pre-graph effect catalog into the new graph runtime so
//! we can do the migration incrementally:
//!
//! - Cog icons on every effect card without per-effect rewrites — the
//!   editor can show a "Source → \<EffectName\> → FinalOutput" snapshot
//!   for any registered effect, even if its internals are still a
//!   monolithic compute shader.
//! - Compositor effect chains can later become a chain of legacy
//!   adapters in a single graph (degenerate sub-graphs that just call
//!   the inner `apply`), unifying the rendering pipeline without
//!   rewriting every effect's body.
//!
//! Each adapter exposes the legacy effect's parameters (synthesized
//! from `EffectMetadata::params`) as graph parameters with matching
//! names, so the same parameter bindings the effect card writes can be
//! routed straight through.
//!
//! Cost is one indirection per `evaluate` call (param flatten + an
//! `EffectInstance` build). For the snapshot-only use case the inner
//! effect is never touched.

use std::sync::OnceLock;

use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;

use crate::effect::{EffectContext, PostProcessEffect};
use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};

/// Type-id prefix for adapter-wrapped legacy effects. The full type id
/// is `legacy.<EffectTypeId>`, so e.g. `legacy.Bloom`. The `legacy.`
/// prefix is the editor's signal that the node's internals are opaque
/// (single compute shader, not a sub-graph) and should be drawn as a
/// box rather than a click-to-expand cluster.
pub const LEGACY_TYPE_ID_PREFIX: &str = "legacy.";

const SOURCE_INPUT: NodeInput = NodePort {
    name: "source",
    ty: PortType::Texture2D,
    kind: PortKind::Input,
    required: true,
};

const OUT_OUTPUT: NodeOutput = NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
};

const LEGACY_INPUTS: [NodeInput; 1] = [SOURCE_INPUT];
const LEGACY_OUTPUTS: [NodeOutput; 1] = [OUT_OUTPUT];

/// Adapter that runs a `Box<dyn PostProcessEffect>` as a graph node.
///
/// `evaluate` resolves the source/output textures from the surrounding
/// graph, flattens the current parameter map into the
/// `Vec<f32>` shape `EffectInstance::param_values` expects, and calls
/// `inner.apply` exactly the way the legacy effect chain would have.
///
/// The adapter is `'static`-data only on construction (other than the
/// inner box) — `metadata` is `&'static`, the param-def cache is
/// derived once and stored, and the synthesized type id is owned.
pub struct LegacyPostProcessNode {
    type_id: EffectNodeType,
    metadata: &'static EffectMetadata,
    inner: Box<dyn PostProcessEffect>,
    /// Cached `ParamDef` list derived from `metadata.params`. Stored
    /// here because `EffectNode::parameters()` returns `&[ParamDef]`
    /// and we need stable storage to hand out a slice.
    params: Box<[ParamDef]>,
}

impl LegacyPostProcessNode {
    pub fn new(
        metadata: &'static EffectMetadata,
        inner: Box<dyn PostProcessEffect>,
    ) -> Self {
        let type_id = EffectNodeType::new(format!(
            "{LEGACY_TYPE_ID_PREFIX}{}",
            metadata.id.as_str()
        ));
        let params = metadata
            .params
            .iter()
            .map(param_spec_to_def)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            type_id,
            metadata,
            inner,
            params,
        }
    }

    /// Borrow the inner effect — used by tests and by host code that
    /// needs to forward `clear_state` / `cleanup_owner_state` calls.
    pub fn inner_mut(&mut self) -> &mut dyn PostProcessEffect {
        self.inner.as_mut()
    }

    pub fn metadata(&self) -> &'static EffectMetadata {
        self.metadata
    }
}

impl EffectNode for LegacyPostProcessNode {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &LEGACY_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &LEGACY_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &self.params
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(source) = ctx.inputs.texture_2d("source") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out.width, out.height);

        // Flatten the current parameter map back into legacy
        // `Vec<f32>` order. The order matches `metadata.params`, which
        // is the same order the legacy effect indexes into via
        // `param_values.first()` / `.get(N)`.
        let param_values: Vec<f32> = self
            .metadata
            .params
            .iter()
            .map(|spec| param_value_to_f32(ctx.params.get(spec.id), spec))
            .collect();

        let mut fx = EffectInstance::new(self.metadata.id.clone());
        fx.param_values = param_values;

        let effect_ctx = EffectContext {
            time: ctx.time.seconds.0 as f32,
            beat: ctx.time.beats.0 as f32,
            dt: ctx.time.delta.0 as f32,
            width,
            height,
            output_width: width,
            output_height: height,
            // owner_key 0 = master scope. Stateful effects share a
            // single owner state across the legacy adapter; this is
            // intentional — adapter graphs run once at the master
            // level, not per-clip.
            owner_key: 0,
            is_clip_level: false,
            edge_stretch_width: 0.0,
            frame_count: 0,
        };

        if self.inner.should_skip(&fx) {
            return;
        }

        let gpu = ctx.gpu_encoder();
        self.inner.apply(gpu, source, out, &fx, &effect_ctx);
    }

    fn clear_state(&mut self) {
        self.inner.clear_state();
    }
}

/// Lookup a registered `EffectMetadata` by its display name.
/// `inventory` collection is one-shot at startup; cache the lookup so
/// repeated callers (snapshot fallback, future graph factories) don't
/// rescan the iterator.
pub fn metadata_by_id(
    id: &manifold_core::EffectTypeId,
) -> Option<&'static EffectMetadata> {
    static MAP: OnceLock<ahash::AHashMap<manifold_core::EffectTypeId, &'static EffectMetadata>> =
        OnceLock::new();
    let map = MAP.get_or_init(|| {
        let mut m: ahash::AHashMap<manifold_core::EffectTypeId, &'static EffectMetadata> =
            ahash::AHashMap::default();
        for meta in inventory::iter::<EffectMetadata> {
            m.insert(meta.id.clone(), meta);
        }
        m
    });
    map.get(id).copied()
}

fn param_spec_to_def(spec: &ParamSpec) -> ParamDef {
    let (ty, default) = if spec.is_toggle {
        (ParamType::Bool, ParamValue::Bool(spec.default_value > 0.5))
    } else if !spec.value_labels.is_empty() {
        (
            ParamType::Enum,
            ParamValue::Enum(spec.default_value.round().max(0.0) as u32),
        )
    } else if spec.whole_numbers {
        (
            ParamType::Int,
            ParamValue::Int(spec.default_value.round() as i32),
        )
    } else {
        (ParamType::Float, ParamValue::Float(spec.default_value))
    };
    // `name` here is the graph-level lookup key (used by
    // `graph.set_param(node, name, ...)` and `ctx.params.get(name)`),
    // so it pairs with the ParamSpec's stable `id`. `label` is the
    // human-readable display string.
    ParamDef {
        name: spec.id,
        label: spec.name,
        ty,
        default,
        range: Some((spec.min, spec.max)),
        enum_values: spec.value_labels,
    }
}

/// Flatten a graph `ParamValue` (or absence) back into the f32 the
/// legacy `EffectInstance::param_values` shape uses. Booleans become
/// 0.0 / 1.0; missing values fall back to the metadata default.
fn param_value_to_f32(value: Option<&ParamValue>, spec: &ParamSpec) -> f32 {
    match value {
        Some(ParamValue::Float(f)) => *f,
        Some(ParamValue::Int(i)) => *i as f32,
        Some(ParamValue::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(ParamValue::Enum(i)) => *i as f32,
        Some(ParamValue::Vec2([x, _])) => *x,
        Some(ParamValue::Vec3([x, _, _])) => *x,
        Some(ParamValue::Vec4([x, _, _, _])) => *x,
        Some(ParamValue::Color([r, _, _, _])) => *r,
        None => spec.default_value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::EffectTypeId;

    static FAKE_PARAMS: [ParamSpec; 2] = [
        ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", ""),
        ParamSpec::whole_labels("mode", "Mode", 0.0, 2.0, 0.0, &["A", "B", "C"], "Mode"),
    ];

    static FAKE_META: EffectMetadata = EffectMetadata {
        id: EffectTypeId::new("FakeLegacyEffect"),
        display_name: "Fake Legacy",
        category: "Post-Process",
        available: true,
        osc_prefix: "fake_legacy",
        legacy_discriminant: None,
        params: &FAKE_PARAMS,
    };

    /// Stand-in PostProcessEffect that records its last-seen param
    /// vector so we can assert the adapter routed values correctly.
    struct ProbeEffect {
        type_id: EffectTypeId,
        last_params: Vec<f32>,
    }

    impl PostProcessEffect for ProbeEffect {
        fn effect_type(&self) -> &EffectTypeId {
            &self.type_id
        }
        fn should_skip(&self, _: &EffectInstance) -> bool {
            false
        }
        fn apply(
            &mut self,
            _gpu: &mut crate::gpu_encoder::GpuEncoder,
            _source: &manifold_gpu::GpuTexture,
            _target: &manifold_gpu::GpuTexture,
            fx: &EffectInstance,
            _ctx: &EffectContext,
        ) {
            self.last_params = fx.param_values.clone();
        }
    }

    #[test]
    fn adapter_synthesizes_param_defs_from_metadata() {
        let probe = Box::new(ProbeEffect {
            type_id: EffectTypeId::new("FakeLegacyEffect"),
            last_params: Vec::new(),
        });
        let node = LegacyPostProcessNode::new(&FAKE_META, probe);

        let defs = node.parameters();
        assert_eq!(defs.len(), 2);
        // `name` on graph-level ParamDef is the stable lookup key —
        // pairs with ParamSpec.id. `label` is the display string.
        assert_eq!(defs[0].name, "amount");
        assert_eq!(defs[0].label, "Amount");
        assert_eq!(defs[0].ty, ParamType::Float);
        assert_eq!(defs[1].name, "mode");
        assert_eq!(defs[1].label, "Mode");
        assert_eq!(defs[1].ty, ParamType::Enum);
        assert_eq!(defs[1].enum_values.len(), 3);
    }

    #[test]
    fn adapter_type_id_is_prefixed() {
        let probe = Box::new(ProbeEffect {
            type_id: EffectTypeId::new("FakeLegacyEffect"),
            last_params: Vec::new(),
        });
        let node = LegacyPostProcessNode::new(&FAKE_META, probe);
        assert_eq!(node.type_id().as_str(), "legacy.FakeLegacyEffect");
    }

    #[test]
    fn param_spec_to_def_handles_continuous_whole_toggle_labels() {
        let cont = ParamSpec::continuous("c", "c", 0.0, 1.0, 0.5, "F2", "");
        assert_eq!(param_spec_to_def(&cont).ty, ParamType::Float);
        let tog = ParamSpec::toggle("t", "t", 0.0, 1.0, 1.0, "");
        assert_eq!(param_spec_to_def(&tog).ty, ParamType::Bool);
        let whole = ParamSpec::whole("w", "w", 0.0, 8.0, 4.0, "");
        assert_eq!(param_spec_to_def(&whole).ty, ParamType::Int);
        let labels = ParamSpec::whole_labels("e", "e", 0.0, 2.0, 0.0, &["x", "y", "z"], "");
        assert_eq!(param_spec_to_def(&labels).ty, ParamType::Enum);
    }
}
