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

    /// Snapshot the graph of a specific registered effect type.
    ///
    /// Resolution order:
    /// 1. Effect overrides `graph_snapshot()` — return that (graph-backed
    ///    effects use this).
    /// 2. Otherwise synthesize a degenerate
    ///    `Source → \<EffectName\> → FinalOutput` snapshot from the
    ///    effect's metadata. Lets the editor canvas show a cog-icon
    ///    view for every effect, even ones still implemented as a
    ///    monolithic compute shader.
    ///
    /// Returns `None` only if the type isn't registered at all.
    pub fn graph_snapshot_for(
        &self,
        type_id: &EffectTypeId,
    ) -> Option<crate::node_graph::GraphSnapshot> {
        let processor = self.processors.get(type_id)?;
        if let Some(snap) = processor.graph_snapshot() {
            return Some(snap);
        }
        let metadata = crate::node_graph::metadata_by_id(type_id)?;
        Some(synthesized_legacy_snapshot(metadata))
    }
}

/// Build a `Source → \<legacy\> → FinalOutput` snapshot for an effect
/// that doesn't expose its own graph. The middle node uses the
/// `legacy.\<EffectTypeId\>` type id (matching `LegacyPostProcessNode`)
/// so the canvas can style it differently from primitive nodes.
fn synthesized_legacy_snapshot(
    metadata: &'static manifold_core::effect_registration::EffectMetadata,
) -> crate::node_graph::GraphSnapshot {
    use crate::node_graph::{
        FINAL_OUTPUT_TYPE_ID, GraphSnapshot, LEGACY_TYPE_ID_PREFIX, NodeSnapshot, PortKindSnapshot,
        PortSnapshot, SOURCE_TYPE_ID, WireSnapshot,
    };

    let source = NodeSnapshot {
        id: 0,
        node_handle: None,
        type_id: SOURCE_TYPE_ID.to_string(),
        title: "Source".to_string(),
        inputs: Vec::new(),
        outputs: vec![PortSnapshot {
            name: "out".to_string(),
            kind: PortKindSnapshot::Texture2D,
        }],
        parameters: Vec::new(),
        editor_pos: None,
    };
    let legacy = NodeSnapshot {
        id: 1,
        node_handle: None,
        type_id: format!("{LEGACY_TYPE_ID_PREFIX}{}", metadata.id.as_str()),
        title: metadata.display_name.to_string(),
        inputs: vec![PortSnapshot {
            name: "source".to_string(),
            kind: PortKindSnapshot::Texture2D,
        }],
        outputs: vec![PortSnapshot {
            name: "out".to_string(),
            kind: PortKindSnapshot::Texture2D,
        }],
        parameters: Vec::new(),
        editor_pos: None,
    };
    let final_out = NodeSnapshot {
        id: 2,
        node_handle: None,
        type_id: FINAL_OUTPUT_TYPE_ID.to_string(),
        title: "Final Output".to_string(),
        inputs: vec![PortSnapshot {
            name: "in".to_string(),
            kind: PortKindSnapshot::Texture2D,
        }],
        outputs: Vec::new(),
        parameters: Vec::new(),
        editor_pos: None,
    };
    let wires = vec![
        WireSnapshot {
            from_node: 0,
            from_port: "out".to_string(),
            to_node: 1,
            to_port: "source".to_string(),
        },
        WireSnapshot {
            from_node: 1,
            from_port: "out".to_string(),
            to_node: 2,
            to_port: "in".to_string(),
        },
    ];
    GraphSnapshot {
        nodes: vec![source, legacy, final_out],
        wires,
    }
}
