use crate::node_graph::freeze::region::census::*;
use crate::node_graph::PrimitiveRegistry;

    /// The invariant `classify_refusal` must never violate: it agrees with
    /// `classify_node` on every node of every bundled preset — `Some(_)` iff
    /// `Boundary`, `None` iff `Eligible`. Runs the (cheap, GPU-free) full
    /// library sweep, so a future classify_node gate added without updating
    /// this file's replica fails LOUD, not as a silently wrong census number.
    #[test]
    fn refusal_census_matches_classify_node() {
        let registry = PrimitiveRegistry::with_builtin();
        let mut checked = 0usize;
        let mut defs: Vec<manifold_core::effect_graph_def::EffectGraphDef> = Vec::new();
        for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(
            manifold_core::preset_def::PresetKind::Effect,
        ) {
            if let Some(view) = crate::node_graph::loaded_preset_view_by_id(&type_id) {
                defs.push((*view.canonical_def).clone());
            }
        }
        for type_id in crate::node_graph::bundled_presets::bundled_preset_type_ids(
            manifold_core::preset_def::PresetKind::Generator,
        ) {
            if let Some(json) = crate::node_graph::bundled_presets::bundled_preset_json(&type_id)
                && let Ok(def) = serde_json::from_str(&json)
            {
                defs.push(def);
            }
        }
        for def in &defs {
            let Ok(flat) = manifold_core::flatten::flatten_groups(def) else { continue;
            };
            for n in &flat.nodes {
                // Graph endpoints are a documented exception: `classify_node`
                // reports them Boundary (they're seams by identity), but
                // `classify_refusal` reports `None` on purpose — a
                // source/final_output isn't a "refusal" in the census sense
                // (there is no lift that would ever fuse a graph's own
                // endpoints away), so it must never inflate the `Other` bucket.
                if n.type_id == SOURCE_TYPE_ID || n.type_id == FINAL_OUTPUT_TYPE_ID {
                    continue;
                }
                let class = classify_node(n, &flat, &registry);
                let refusal = classify_refusal(n, &flat, &registry);
                match (&class, &refusal) {
                    (NodeClass::Eligible, None) | (NodeClass::Boundary, Some(_)) => {}
                    _ => panic!(
                        "classify_refusal disagrees with classify_node on node {} ({}): class={class:?} refusal={refusal:?}",
                        n.id, n.type_id
                    ),
                }
                checked += 1;
            }
        }
        assert!(checked > 100, "sweep too small to trust ({checked} nodes) — bundled preset enumeration broke");
    }

use crate::node_graph::{SOURCE_TYPE_ID, FINAL_OUTPUT_TYPE_ID};
use crate::node_graph::freeze::region::{classify_node, NodeClass};