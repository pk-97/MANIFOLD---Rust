    //! Project-load segment prewarm shares `classify_segment_member` /
    //! `segment_run` with the chain build — these lock the shared pieces.
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    #[test]
    fn segment_run_collects_fuse_indices_until_boundary() {
        use SegmentMember::{Boundary, Fuse};
        // Fuse, Fuse, Boundary: run ends at the boundary, fuse idxs = [0, 1].
        let members = [Fuse, Fuse, Boundary];
        let (j, fuse) = segment_run(&members, 0);
        assert_eq!(j, 2);
        assert_eq!(fuse, vec![0, 1]);
    }

    #[test]
    fn prewarm_classifies_stateless_cards_fuse_and_enqueues_without_panicking() {
        let primitives = PrimitiveRegistry::with_builtin();
        let a = make_default(PresetTypeId::COLOR_GRADE);
        let b = make_default(PresetTypeId::INVERT_COLORS);
        assert_eq!(
            classify_segment_member(&a, None, &primitives),
            SegmentMember::Fuse,
            "ColorGrade is a stateless ungrouped card — segment member"
        );
        // A watched card is a boundary — prewarm passes None so nothing is
        // watched at load, but the build-time exclusion must hold.
        assert_eq!(
            classify_segment_member(&a, Some(&a.id), &primitives),
            SegmentMember::Boundary
        );
        // Enqueue-only walk; in cfg(test) the segment lookup stays Pending
        // (no worker) — this locks that the walk itself is panic-free and
        // exercises the same card-list construction as the build.
        prewarm_chain_segments(&[a, b], &[], &primitives);
    }
