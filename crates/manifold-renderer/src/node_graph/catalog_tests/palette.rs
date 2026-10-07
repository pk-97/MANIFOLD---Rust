use crate::node_graph::palette::catalog_graph_def_for;

    #[test]
    fn catalog_default_is_available_for_every_shipping_effect() {
        // Previously only Mirror + SoftFocus had catalog graphs;
        // the bundled-preset registry now covers every ChainSpec, so
        // per-card divergence works on every effect.
        for type_id in
            crate::node_graph::bundled_preset_type_ids(manifold_core::preset_def::PresetKind::Effect)
        {
            assert!(
                catalog_graph_def_for(&type_id).is_some(),
                "missing catalog default for shipping effect {}",
                type_id.as_str(),
            );
        }
    }
