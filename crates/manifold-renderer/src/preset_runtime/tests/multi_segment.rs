    //! Regression tests for the multi-segment wet/dry group support in
    //! `PresetRuntime::try_build`. A "multi-segment" group is one whose
    //! enabled effects sit in non-contiguous positions in the chain —
    //! e.g. group `g` contains effects at indices 0 and 2, with a
    //! non-group effect at index 1 between them.
    //!
    //! Pre-fix: `try_build` rejected this layout via the
    //! `enabled_groups_are_contiguous` preflight; the chain fell back
    //! to the legacy per-effect dispatcher.
    //!
    //! Post-fix: the build loop's open/close-on-every-transition
    //! pattern emits one Mix sub-graph per segment, each fed from the
    //! pre-segment output and feeding the post-segment input. All Mix
    //! nodes register under the same `EffectGroupId` in
    //! `group_mix_nodes`, so the per-frame `wet_dry` refresh sets the
    //! `amount` param on every segment uniformly.
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::{EffectGroup, PresetInstance};
    use manifold_core::id::EffectGroupId;
    

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    #[test]
    fn non_contiguous_group_builds_multi_segment_mix() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let g1_id = EffectGroupId::new("g1");

        // Chain: Invert(g1) → ChromaticAberration → Invert(g1)
        // Effects on either side belong to g1; the middle effect doesn't.
        let mut e1 = make_default(PresetTypeId::INVERT_COLORS);
        e1.group_id = Some(g1_id.clone());
        let e2 = make_default(PresetTypeId::CHROMATIC_ABERRATION);
        let mut e3 = make_default(PresetTypeId::INVERT_COLORS);
        e3.group_id = Some(g1_id.clone());

        let g1 = EffectGroup {
            id: g1_id.clone(),
            name: "g1".to_string(),
            enabled: true,
            collapsed: false,
            wet_dry: 0.5,
            parent_group_id: None,
        };

        let result =
            PresetRuntime::try_build(ChainBuildInputs { effects: &[e1, e2, e3], groups: &[g1], primitives: &primitives, device: &device, pool: None, width: 256, height: 256, preview_effect: None }, None);

        let cg = result.expect(
            "PresetRuntime should build for a non-contiguous wet/dry group \
             (multi-segment Mix support)",
        );

        // Two segments → two Mix sub-graphs, both keyed to g1.
        assert_eq!(
            cg.group_mix_nodes.len(),
            2,
            "non-contiguous group with 2 segments must emit 2 Mix sub-graphs",
        );
        for (gid, _) in &cg.group_mix_nodes {
            assert_eq!(gid.as_str(), "g1");
        }
    }

    #[test]
    fn contiguous_group_still_builds_single_mix() {
        // Regression guard: the contiguous case still produces exactly
        // one Mix sub-graph.
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let g1_id = EffectGroupId::new("g1");

        let mut e1 = make_default(PresetTypeId::INVERT_COLORS);
        e1.group_id = Some(g1_id.clone());
        let mut e2 = make_default(PresetTypeId::CHROMATIC_ABERRATION);
        e2.group_id = Some(g1_id.clone());
        let e3 = make_default(PresetTypeId::INVERT_COLORS);

        let g1 = EffectGroup {
            id: g1_id.clone(),
            name: "g1".to_string(),
            enabled: true,
            collapsed: false,
            wet_dry: 0.5,
            parent_group_id: None,
        };

        let result =
            PresetRuntime::try_build(ChainBuildInputs { effects: &[e1, e2, e3], groups: &[g1], primitives: &primitives, device: &device, pool: None, width: 256, height: 256, preview_effect: None }, None);

        let cg = result.expect("PresetRuntime should build for contiguous group");
        assert_eq!(cg.group_mix_nodes.len(), 1);
    }

    #[test]
    fn three_segment_group_builds_three_mix_sub_graphs() {
        // Chain: Invert(g1) → Chroma → Invert(g1) → Chroma → Invert(g1)
        // Group g1 has three non-contiguous segments.
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let g1_id = EffectGroupId::new("g1");
        let mut e1 = make_default(PresetTypeId::INVERT_COLORS);
        e1.group_id = Some(g1_id.clone());
        let e2 = make_default(PresetTypeId::CHROMATIC_ABERRATION);
        let mut e3 = make_default(PresetTypeId::INVERT_COLORS);
        e3.group_id = Some(g1_id.clone());
        let e4 = make_default(PresetTypeId::CHROMATIC_ABERRATION);
        let mut e5 = make_default(PresetTypeId::INVERT_COLORS);
        e5.group_id = Some(g1_id.clone());

        let g1 = EffectGroup {
            id: g1_id.clone(),
            name: "g1".to_string(),
            enabled: true,
            collapsed: false,
            wet_dry: 0.3,
            parent_group_id: None,
        };

        let result = PresetRuntime::try_build(ChainBuildInputs { effects: &[e1, e2, e3, e4, e5], groups: &[g1], primitives: &primitives, device: &device, pool: None, width: 256, height: 256, preview_effect: None }, None);

        let cg = result.expect("PresetRuntime should build for three-segment group");
        assert_eq!(cg.group_mix_nodes.len(), 3);
    }
