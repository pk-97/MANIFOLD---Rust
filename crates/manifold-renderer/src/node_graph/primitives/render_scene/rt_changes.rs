use crate::node_graph::{MeshRevision, Slot};
use manifold_gpu::raytrace::RtGeometryChange;

/// Classify the per-object acceleration-structure update for one evaluated
/// frame. Connectivity changes rebuild; position-only changes refit the
/// resident hierarchy and update its instance bounds on the same encoder.
pub(super) fn classify_mesh_change(
    previous: Option<(MeshRevision, Option<(Slot, u64)>)>,
    current: Option<MeshRevision>,
    topology_hint: Option<(Slot, u64)>,
    structural_changed: bool,
) -> RtGeometryChange {
    if structural_changed {
        return RtGeometryChange::Rebuild;
    }

    let Some((previous_revision, previous_topology_hint)) = previous else {
        return RtGeometryChange::Rebuild;
    };
    let Some(current_revision) = current else {
        return RtGeometryChange::Rebuild;
    };

    if previous_topology_hint != topology_hint
        || previous_revision.topology != current_revision.topology
    {
        RtGeometryChange::Rebuild
    } else if previous_revision.positions != current_revision.positions {
        RtGeometryChange::Refit
    } else if previous_revision.content != current_revision.content {
        RtGeometryChange::Attributes
    } else {
        RtGeometryChange::Reuse
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revision(topology: u64, positions: u64, content: u64) -> MeshRevision {
        MeshRevision { topology, positions, content }
    }

    fn slot_hint(slot: u32, generation: u64) -> Option<(Slot, u64)> {
        Some((Slot(slot), generation))
    }

    #[test]
    fn first_or_missing_revision_metadata_rebuilds() {
        let rev = revision(1, 1, 1);
        assert_eq!(
            classify_mesh_change(None, Some(rev), None, false),
            RtGeometryChange::Rebuild
        );
        assert_eq!(
            classify_mesh_change(Some((rev, None)), None, None, false),
            RtGeometryChange::Rebuild
        );
        assert_eq!(
            classify_mesh_change(None, None, None, false),
            RtGeometryChange::Rebuild
        );
        assert_eq!(
            classify_mesh_change(Some((rev, None)), Some(rev), None, true),
            RtGeometryChange::Rebuild
        );
    }

    #[test]
    fn topology_revision_or_hint_change_rebuilds() {
        let hint = slot_hint(2, 7);
        let previous = revision(1, 1, 1);
        assert_eq!(
            classify_mesh_change(
                Some((previous, hint)),
                Some(revision(2, 1, 1)),
                hint,
                false,
            ),
            RtGeometryChange::Rebuild
        );
        assert_eq!(
            classify_mesh_change(
                Some((previous, hint)),
                Some(previous),
                slot_hint(2, 8),
                false,
            ),
            RtGeometryChange::Rebuild
        );
    }

    #[test]
    fn position_change_refits_with_stable_topology() {
        let previous = revision(1, 1, 1);
        assert_eq!(
            classify_mesh_change(
                Some((previous, None)),
                Some(revision(1, 2, 1)),
                None,
                false,
            ),
            RtGeometryChange::Refit
        );
    }

    #[test]
    fn content_only_change_refreshes_attributes() {
        let previous = revision(1, 1, 1);
        assert_eq!(
            classify_mesh_change(
                Some((previous, None)),
                Some(revision(1, 1, 2)),
                None,
                false,
            ),
            RtGeometryChange::Attributes
        );
    }

    #[test]
    fn unchanged_revision_and_hint_reuse() {
        let revision = revision(3, 4, 5);
        let hint = slot_hint(6, 7);
        assert_eq!(
            classify_mesh_change(Some((revision, hint)), Some(revision), hint, false),
            RtGeometryChange::Reuse
        );
    }
}
