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
        // actually produces. Typos fail at process boot instead of
        // silently dropping params at first render.
        for err in crate::node_graph::validate_all_specs() {
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

    /// Snapshot the graph of a specific registered effect type.
    ///
    /// Resolution order:
    /// 1. Effect declares a [`ChainSpec`] — build its canonical graph
    ///    via `spec.build_canonical_graph()` and return that snapshot.
    /// 2. Effect overrides `PostProcessEffect::graph_snapshot()` —
    ///    return that (graph-backed effects use this until migrated).
    /// 3. Otherwise synthesize a degenerate
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
        // Path 1: ChainSpec. Authoritative once an effect has migrated.
        if let Some(spec) = crate::node_graph::chain_spec_by_id(type_id) {
            let g = spec.build_canonical_graph();
            let mut snap = crate::node_graph::GraphSnapshot::from_graph(&g);
            snap.outer_routings = outer_routings_from_spec(spec, &g);
            return Some(snap);
        }
        // Path 2: legacy graph-backed PostProcessEffect (un-migrated
        // graph-backed effects like SoftFocus, StylizedFeedback).
        let processor = self.processors.get(type_id)?;
        if let Some(mut snap) = processor.graph_snapshot() {
            snap.outer_routings = processor.outer_param_routings();
            return Some(snap);
        }
        // Path 3: legacy single-pass effect — synthesized placeholder.
        let metadata = crate::node_graph::metadata_by_id(type_id)?;
        Some(synthesized_legacy_snapshot(metadata))
    }

    /// Outer→inner routings declared by `type_id`'s registered
    /// effect. Used by the content thread for the per-card
    /// (`from_def`) snapshot path, where the snapshot is built off a
    /// serialized graph and `graph_snapshot_for` isn't called.
    pub fn outer_routings_for(
        &self,
        type_id: &EffectTypeId,
    ) -> Vec<crate::node_graph::OuterParamRouting> {
        if let Some(spec) = crate::node_graph::chain_spec_by_id(type_id) {
            let g = spec.build_canonical_graph();
            return outer_routings_from_spec(spec, &g);
        }
        self.processors
            .get(type_id)
            .map(|p| p.outer_param_routings())
            .unwrap_or_default()
    }
}

/// Translate a [`ChainSpec`]'s routings into [`OuterParamRouting`]s
/// the editor inspector consumes (which inner rows to gray out as
/// "driven by '<outer>'"). One entry per spec routing whose target
/// handle resolves on the just-built canonical graph.
fn outer_routings_from_spec(
    spec: &'static crate::node_graph::ChainSpec,
    graph: &crate::node_graph::Graph,
) -> Vec<crate::node_graph::OuterParamRouting> {
    use crate::node_graph::OuterParamRouting;
    let Some(metadata) = crate::node_graph::metadata_by_id(&spec.type_id) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(spec.routings.len());
    for routing in spec.routings {
        let Some(param_spec) = metadata.params.iter().find(|p| p.id == routing.param_id) else {
            continue;
        };
        // Resolve target handle into the canonical-graph node id, then
        // recover that node's stable handle string for the editor. For
        // canonical specs the node is added without a global handle —
        // we have to walk the graph's handles map to find one if it
        // was registered, or fall back to the spec's local name.
        let canonical_handles: Vec<(String, crate::node_graph::NodeInstanceId)> = graph
            .handles()
            .map(|(h, id)| (h.to_string(), id))
            .collect();
        // The editor key for `node_handle` is whatever the canvas
        // shows the user. Splice doesn't register global handles, so
        // for now we surface the effect-local handle name directly —
        // the editor uses it to look up the corresponding node in the
        // snapshot via `NodeSnapshot.node_handle` which IS set when
        // a graph node has a registered handle.
        //
        // For ChainSpec'd effects whose splice uses `add_node` (no
        // global handle), the snapshot's node_handle is None and the
        // editor falls back to node type id for display. The routing
        // information here still tells the editor *which* outer name
        // drives which inner param.
        let _ = canonical_handles;
        out.push(OuterParamRouting {
            outer_label: param_spec.name.to_string(),
            node_handle: routing.target_handle.to_string(),
            inner_param: routing.target_param.to_string(),
        });
    }
    out
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
        // Legacy-wrapped effects don't surface any outer→inner
        // routing — their parameters live on the wrapped node and
        // are edited from the effect card directly, not through a
        // composite-handle indirection. Nothing to gray out.
        outer_routings: Vec::new(),
    }
}
