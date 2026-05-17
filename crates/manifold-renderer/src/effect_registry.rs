use crate::effect::PostProcessEffect;
use manifold_core::EffectTypeId;
use manifold_gpu::GpuDevice;
use std::collections::HashMap;

/// Factory + singleton storage for legacy effect processors. Retained
/// post–legacy-dispatch removal for two remaining roles:
///
/// 1. **Editor snapshot lookup** ([`graph_snapshot_for`]) — every
///    registered legacy effect can synthesize a
///    `Source → <Effect> → FinalOutput` snapshot for the canvas.
/// 2. **Export warmup** ([`flush_all_background_work`]) — plugin-using
///    legacy effects (DepthEstimator / BlobDetector) hold background
///    worker handles whose in-flight work must be drained before each
///    export frame for determinism.
///
/// The previous per-effect dispatch path and per-owner state cache are
/// gone — chains run exclusively through `ChainGraph`. See
/// `docs/EFFECT_CHAIN_LIFECYCLE.md`.
///
/// All effects are registered via `inventory::submit!` in their
/// implementation files.
pub struct EffectRegistry {
    processors: HashMap<EffectTypeId, Box<dyn PostProcessEffect>>,
}

impl EffectRegistry {
    pub fn new(device: &GpuDevice) -> Self {
        let mut processors: HashMap<EffectTypeId, Box<dyn PostProcessEffect>> = HashMap::new();

        // Collect all inventory-registered effects
        for factory in inventory::iter::<crate::effects::registration::EffectFactory> {
            processors.insert(factory.id.clone(), (factory.create)(device));
        }

        // Validate every registered ChainSpec at startup — every
        // routing must reference a handle that the spec's splice
        // actually produces, and every binding's spec must match
        // its EffectMetadata.params entry. Typos and metadata drift
        // fail at process boot instead of silently dropping params
        // at first render.
        for err in crate::node_graph::validate_all_specs() {
            eprintln!("[manifold-renderer] {err}");
        }
        for err in crate::node_graph::validate_binding_spec_parity() {
            eprintln!("[manifold-renderer] {err}");
        }

        Self { processors }
    }

    /// Resize all processors to new dimensions. Called on render-
    /// resolution change so any internal pipelines / scratch buffers
    /// inside the singletons stay in sync — even though apply() never
    /// fires on them, some effects key their snapshot output on
    /// dimensions, and the background workers may need to know.
    pub fn resize_all(&mut self, device: &GpuDevice, width: u32, height: u32) {
        for processor in self.processors.values_mut() {
            processor.resize(device, width, height);
        }
    }

    /// Flush all in-flight background work across all processors.
    /// Called after each export frame for deterministic async pipeline resolution.
    pub fn flush_all_background_work(&mut self) {
        for processor in self.processors.values_mut() {
            processor.flush_background_work();
        }
    }

    /// Snapshot the canonical graph of a registered effect for the
    /// editor canvas. Every shipping effect declares a `ChainSpec`,
    /// so resolution is a single path: build the spec's standalone
    /// graph and project its routings onto the snapshot.
    pub fn graph_snapshot_for(
        &self,
        type_id: &EffectTypeId,
    ) -> Option<crate::node_graph::GraphSnapshot> {
        let spec = crate::node_graph::chain_spec_by_id(type_id)?;
        let g = spec.build_canonical_graph();
        let mut snap = crate::node_graph::GraphSnapshot::from_graph(&g);
        snap.outer_routings = outer_routings_from_spec(spec, &g);
        Some(snap)
    }

    /// Outer→inner routings declared by `type_id`'s registered
    /// effect. Used by the content thread for the per-card
    /// (`from_def`) snapshot path, where the snapshot is built off a
    /// serialized graph and `graph_snapshot_for` isn't called.
    pub fn outer_routings_for(
        &self,
        type_id: &EffectTypeId,
    ) -> Vec<crate::node_graph::OuterParamRouting> {
        let Some(spec) = crate::node_graph::chain_spec_by_id(type_id) else {
            return Vec::new();
        };
        let g = spec.build_canonical_graph();
        outer_routings_from_spec(spec, &g)
    }
}

/// Translate a [`ChainSpec`]'s bindings into [`OuterParamRouting`]s
/// the editor inspector consumes (which inner rows to gray out as
/// "driven by '<outer>'"). One entry per binding whose target handle
/// is reachable.
///
/// Bindings carry their `HandleNode { handle, param }` directly, so
/// no metadata lookup is needed. Composite-style bindings or
/// `Custom(fn)` targets are skipped — the editor can't surface them.
fn outer_routings_from_spec(
    spec: &'static crate::node_graph::ChainSpec,
    _graph: &crate::node_graph::Graph,
) -> Vec<crate::node_graph::OuterParamRouting> {
    use crate::node_graph::{OuterParamRouting, ParamTarget};
    let mut out = Vec::with_capacity(spec.bindings.len());
    for binding in spec.bindings {
        let (handle, inner_param) = match &binding.target {
            ParamTarget::HandleNode { handle, param } => (*handle, *param),
            // Composite / Node / Custom variants either don't surface
            // a handle at spec-time or aren't relevant to the editor's
            // outer→inner gray-out display.
            _ => continue,
        };
        out.push(OuterParamRouting {
            outer_label: binding.label.to_string(),
            outer_param_id: binding.id.to_string(),
            node_handle: handle.to_string(),
            inner_param: inner_param.to_string(),
            // This walk operates on registry-side `ChainSpec.bindings`
            // — every entry is a compile-time spec binding.
            source: crate::node_graph::OuterParamSource::Static,
        });
    }
    out
}

