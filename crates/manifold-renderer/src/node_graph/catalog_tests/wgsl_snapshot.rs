

    /// Deterministic dump of every fused `node.wgsl_compute` kernel's WGSL text,
    /// across every bundled effect + generator preset — sorted so re-runs are
    /// byte-stable. This is the P1 hard gate's raw material: the marker refactor
    /// (D1) must change zero emitted bytes, and the WGSL text is the
    /// cross-session pipeline-cache key, so "zero bytes changed" is checked at
    /// the text level, not "compiles" or "renders the same".
    fn capture_all_fused_wgsl() -> String {
        use crate::node_graph::PrimitiveRegistry;
        use crate::node_graph::freeze::install::{fuse_canonical_def, fuse_generator_view};
        use manifold_core::effect_graph_def::EffectGraphDef;
        use manifold_core::preset_def::PresetKind;

        let registry = PrimitiveRegistry::with_builtin();
        let mut out = String::new();

        let mut effect_ids: Vec<_> =
            crate::node_graph::bundled_presets::bundled_preset_type_ids(PresetKind::Effect)
                .collect();
        effect_ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for type_id in effect_ids {
            let Some(view) = crate::node_graph::loaded_preset_view_by_id(&type_id) else {
                continue;
            };
            let Some(fused) = fuse_canonical_def(&view.canonical_def, &registry) else {
                continue;
            };
            let mut nodes: Vec<_> =
                fused.def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").collect();
            nodes.sort_by_key(|n| n.id);
            for node in nodes {
                if let Some(wgsl) = &node.wgsl_source {
                    out.push_str(&format!("=== effect:{} node:{} ===\n", type_id.as_str(), node.id));
                    out.push_str(wgsl);
                    out.push('\n');
                }
            }
        }

        let mut gen_ids: Vec<_> =
            crate::node_graph::bundled_presets::bundled_preset_type_ids(PresetKind::Generator)
                .collect();
        gen_ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for type_id in gen_ids {
            let Some(json) = crate::node_graph::bundled_presets::bundled_preset_json(&type_id)
            else {
                continue;
            };
            let Ok(def) = serde_json::from_str::<EffectGraphDef>(&json) else { continue };
            let Some(fused_view) = fuse_generator_view(&def, &registry) else { continue };
            let mut nodes: Vec<_> =
                fused_view.def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").collect();
            nodes.sort_by_key(|n| n.id);
            for node in nodes {
                if let Some(wgsl) = &node.wgsl_source {
                    out.push_str(&format!(
                        "=== generator:{} node:{} ===\n",
                        type_id.as_str(),
                        node.id
                    ));
                    out.push_str(wgsl);
                    out.push('\n');
                }
            }
        }
        out
    }

    fn golden_path() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fused_wgsl_snapshot.txt")
    }

    /// P1 hard gate (FUSION_SOTA_DESIGN D1): the marker refactor must emit
    /// byte-identical WGSL for every bundled preset. The golden fixture was
    /// captured from origin/main HEAD (6888ea28, pre-refactor codegen) by
    /// temporarily stashing only `freeze/codegen.rs` + `freeze/install.rs` (the
    /// emit sites), running this test with `UPDATE_FUSION_GOLDEN=1`, then
    /// restoring the refactor and re-running normally. Regenerate the fixture
    /// (`UPDATE_FUSION_GOLDEN=1 cargo test …`) only for an INTENTIONAL codegen
    /// change — never to make this phase's refactor pass.
    #[test]
    fn fused_wgsl_snapshot_unchanged() {
        let actual = capture_all_fused_wgsl();
        let path = golden_path();
        if std::env::var("UPDATE_FUSION_GOLDEN").is_ok() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &actual).unwrap();
            return;
        }
        let golden = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "missing golden fixture at {path:?} — run with UPDATE_FUSION_GOLDEN=1 to create it"
            )
        });
        assert_eq!(
            actual, golden,
            "fused WGSL text changed for at least one bundled preset — the marker \
             refactor (D1) must emit byte-identical output (the WGSL text is the \
             pipeline-cache key). If this change is intentional (a different phase's \
             codegen work), regenerate with UPDATE_FUSION_GOLDEN=1."
        );
    }
