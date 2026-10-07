use manifold_node_engine::freeze::fusion_report::*;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::freeze::region;
use manifold_core::flatten::flatten_groups;
use manifold_core::effect_graph_def::EffectGraphDef;

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
                    got.member_node_ids,
                    want_members,
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
