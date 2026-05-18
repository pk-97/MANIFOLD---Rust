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
    /// editor canvas. Sourced from the JSON-loaded
    /// [`crate::node_graph::LoadedPresetView`] — block 7 rewired this
    /// off the legacy `ChainSpec` path so editor previews share the
    /// same authoritative metadata the chain runtime reads.
    pub fn graph_snapshot_for(
        &self,
        type_id: &EffectTypeId,
    ) -> Option<crate::node_graph::GraphSnapshot> {
        let view = crate::node_graph::loaded_preset_view_by_id(type_id)?;
        crate::node_graph::snapshot_for_view(view)
    }

    /// Outer→inner routings declared by `type_id`'s registered
    /// effect. Used by the content thread for the per-card
    /// (`from_def`) snapshot path, where the snapshot is built off a
    /// serialized graph and `graph_snapshot_for` isn't called.
    pub fn outer_routings_for(
        &self,
        type_id: &EffectTypeId,
    ) -> Vec<crate::node_graph::OuterParamRouting> {
        let Some(view) = crate::node_graph::loaded_preset_view_by_id(type_id) else {
            return Vec::new();
        };
        crate::node_graph::outer_routings_from_view(view)
    }
}

