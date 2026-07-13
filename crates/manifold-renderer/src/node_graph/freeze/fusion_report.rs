//! Fusion report — per-node classification + region summary for `graph_tool
//! fusion` (GRAPH_TOOLING_DESIGN D2/D10, §3 "Committed shapes").
//!
//! Calls [`region::partition_regions`] — the exact pure function the freeze
//! pipeline itself calls to grow fusion regions — and reuses its private
//! per-node classifier ([`region::classify_node`], exposed `pub(crate)` for
//! this reader) to explain non-member nodes. Neither is reimplemented here;
//! this module only reads their output and renders it (D1's "one
//! implementation, every consumer reads it" principle applied to fusion,
//! same as `validate_def` applies it to load/compile).
//!
//! **D10 — flatten before partitioning.** `partition_regions` returns an
//! empty region list for a def still carrying group nodes
//! (`region.rs`, `partition_regions`'s own group-node early-return) — the
//! runtime loader flattens groups before fusion ever sees a def
//! (`graph_loader.rs`'s `into_graph`, guarded the same way: only flattens
//! when a group node is actually present). This module mirrors that exact
//! order so a grouped preset reports its real region count, never a false
//! "0 regions".

use ahash::AHashMap;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::flatten::flatten_groups;

use crate::node_graph::PrimitiveRegistry;
use crate::node_graph::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, SOURCE_TYPE_ID};
use crate::node_graph::freeze::classify::fusion_kind_str;
use crate::node_graph::freeze::region::{self, NodeClass, Region};

/// One node's fusion classification within a specific (flattened) def.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct NodeFusionInfo {
    pub node_id: u32,
    pub type_id: String,
    /// This node's own declared classification, rendered the same way
    /// `catalog_gen`'s `fusion` field is: `"pointwise"` | `"source"` |
    /// `"multi_input_coincident"` | `"boundary:<reason_snake_case>"`.
    pub kind: String,
    /// `true` iff this node is a member of one of [`FusionReport::regions`].
    pub fused: bool,
    /// Index into [`FusionReport::regions`], when `fused`.
    pub region_index: Option<usize>,
    /// One-line reason this node sits outside every region. `None` iff `fused`.
    pub cut_reason: Option<String>,
}

/// One fusion region — the members that fold into a single dispatch.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RegionSummary {
    pub member_node_ids: Vec<u32>,
    pub external_count: usize,
    pub output_count: usize,
}

/// Full report for one graph document.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct FusionReport {
    pub nodes: Vec<NodeFusionInfo>,
    pub regions: Vec<RegionSummary>,
    /// `regions.len()` + every node that is NOT a region member — the
    /// dispatch count the fused graph would issue, vs. one dispatch per
    /// node unfused.
    pub estimated_dispatch_count: usize,
}

/// Build the report for `def`. Flattens groups first (D10), then calls
/// [`region::partition_regions`] once — the SAME call the freeze pipeline
/// makes — and classifies every remaining node for context.
pub fn fusion_report(def: &EffectGraphDef, registry: &PrimitiveRegistry) -> FusionReport {
    // Loader parity (D10): `flatten_groups` is a cheap clone when nothing
    // needs flattening (its own fast path), so this costs nothing for the
    // overwhelming majority of already-flat presets and is required for
    // every grouped one.
    let flat = match flatten_groups(def) {
        Ok(f) => f,
        Err(_) => def.clone(),
    };

    let regions: Vec<Region> = region::partition_regions(&flat, registry);

    let mut member_of: AHashMap<u32, usize> = AHashMap::default();
    for (idx, r) in regions.iter().enumerate() {
        for m in &r.members {
            member_of.insert(m.doc_id, idx);
        }
    }

    let nodes: Vec<NodeFusionInfo> = flat
        .nodes
        .iter()
        .map(|n| {
            let kind = registry
                .construct(&n.type_id)
                .map(|inst| fusion_kind_str(inst.as_ref()))
                .unwrap_or_else(|| "boundary:unknown_type".to_string());
            let region_index = member_of.get(&n.id).copied();
            let fused = region_index.is_some();
            let cut_reason = if fused {
                None
            } else {
                Some(cut_reason_for(n, &flat, registry))
            };
            NodeFusionInfo {
                node_id: n.id,
                type_id: n.type_id.clone(),
                kind,
                fused,
                region_index,
                cut_reason,
            }
        })
        .collect();

    let regions_out: Vec<RegionSummary> = regions
        .iter()
        .map(|r| RegionSummary {
            member_node_ids: r.members.iter().map(|m| m.doc_id).collect(),
            external_count: r.externals.len(),
            output_count: r.outputs.len(),
        })
        .collect();

    let boundary_node_count = flat.nodes.len() - member_of.len();
    let estimated_dispatch_count = regions_out.len() + boundary_node_count;

    FusionReport {
        nodes,
        regions: regions_out,
        estimated_dispatch_count,
    }
}

/// One-line explanation for why `node` isn't a member of any region.
/// Re-runs [`region::classify_node`] (the library's own per-node gate) to
/// distinguish a hard cut (Boundary) from an eligible atom that simply
/// wasn't grouped (isolated / gated by a union or region-build rule) —
/// never re-derives the gate logic itself.
fn cut_reason_for(
    node: &manifold_core::effect_graph_def::EffectGraphNode,
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> String {
    if node.type_id == SOURCE_TYPE_ID || node.type_id == FINAL_OUTPUT_TYPE_ID {
        return "graph endpoint (system.source/final_output never fuses)".to_string();
    }

    match region::classify_node(node, def, registry) {
        NodeClass::Boundary => {
            let own_kind = registry
                .construct(&node.type_id)
                .map(|inst| fusion_kind_str(inst.as_ref()));
            match own_kind.as_deref() {
                Some(k) if k.starts_with("boundary:") => {
                    format!("declared {k} (see docs/ADDING_PRIMITIVES.md exemption taxonomy)")
                }
                Some(_) => "fusable atom, cut by a graph-specific gate (unwired/optional input, \
                             non-scalar param, texture arity, resample scale, or space mismatch \
                             — see docs/FREEZE_COMPILER_MAP.md §4 cut rules 2-9)"
                    .to_string(),
                None => "unknown type_id — never fuses".to_string(),
            }
        }
        NodeClass::Eligible => "eligible atom not grouped into any region (isolated, or excluded \
                                 by a union/region-build gate — see docs/FREEZE_COMPILER_MAP.md \
                                 §4 union/region gates, MIN_REGION_LEN=2)"
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::PrimitiveRegistry;

    /// Ground-truth gate (P3): the verb's region count + membership must be
    /// bit-identical to calling the freeze pipeline's own
    /// `flatten_groups` → `partition_regions` directly — the same library
    /// calls, machine-compared, never eyeballed.
    #[test]
    fn fusion_verb_matches_freeze_partition() {
        let registry = PrimitiveRegistry::with_builtin();
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = manifest_dir.join("assets/effect-presets");
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));

        let mut checked = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: read failed: {e}", path.display()));
            let def: EffectGraphDef = serde_json::from_str(&bytes)
                .unwrap_or_else(|e| panic!("{}: parse failed: {e}", path.display()));

            let report = fusion_report(&def, &registry);

            // Ground truth: flatten (loader parity) + partition, directly.
            let flat = flatten_groups(&def).unwrap_or_else(|_| def.clone());
            let ground_truth = region::partition_regions(&flat, &registry);

            assert_eq!(
                report.regions.len(),
                ground_truth.len(),
                "{}: region COUNT mismatch between graph_tool fusion and the real freeze partition",
                path.display()
            );
            for (got, want) in report.regions.iter().zip(ground_truth.iter()) {
                let want_members: Vec<u32> = want.members.iter().map(|m| m.doc_id).collect();
                assert_eq!(
                    got.member_node_ids, want_members,
                    "{}: region MEMBERSHIP mismatch",
                    path.display()
                );
            }
            checked += 1;
        }
        assert!(checked > 0, "expected to find bundled effect presets");
    }

    /// A grouped preset must NOT falsely report zero regions (D10's known
    /// wrong answer) — the report's own flatten must run before partition.
    #[test]
    fn grouped_preset_reports_real_regions_not_zero() {
        let registry = PrimitiveRegistry::with_builtin();
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = manifest_dir.join("assets/effect-presets");
        let entries = std::fs::read_dir(&dir).expect("read effect-presets dir");

        let mut found_grouped_with_regions = false;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read_to_string(&path).expect("read preset");
            let def: EffectGraphDef = serde_json::from_str(&bytes).expect("parse preset");
            if !def.nodes.iter().any(|n| n.group.is_some()) {
                continue;
            }
            let report = fusion_report(&def, &registry);
            if !report.regions.is_empty() {
                found_grouped_with_regions = true;
            }
        }
        assert!(
            found_grouped_with_regions,
            "expected at least one grouped bundled preset to report non-zero regions \
             (a 0-region report for every grouped preset is the D10 false-answer bug)"
        );
    }
}
