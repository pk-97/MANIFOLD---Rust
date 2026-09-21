use crate::node_graph::{ContentVersion, MeshRevision};
use manifold_gpu::raytrace::RtGeometryChange;

/// Classify the per-object acceleration-structure update for one evaluated
/// frame. Connectivity changes rebuild; position-only changes refit the
/// resident hierarchy and update its instance bounds on the same encoder.
pub(super) fn classify_mesh_change(
    previous: Option<(MeshRevision, Option<ContentVersion>)>,
    current: Option<MeshRevision>,
    topology_hint: Option<ContentVersion>,
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

/// Semantic appearance keys accept logical versions, never storage revisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AppearanceKey(u64);

#[derive(Default)]
pub(super) struct AppearanceKeyBuilder(ahash::AHasher);

impl AppearanceKeyBuilder {
    pub(super) fn content(&mut self, content: Option<ContentVersion>) {
        use std::hash::Hash;
        content.hash(&mut self.0);
    }

    pub(super) fn parameter_bytes(&mut self, bytes: &[u8]) {
        use std::hash::Hasher;
        self.0.write(bytes);
    }

    pub(super) fn finish(self) -> AppearanceKey {
        use std::hash::Hasher;
        AppearanceKey(self.0.finish())
    }
}

/// One classification drives history and the baked emissive table. Instance
/// motion has its existing reprojection path and GPU gather-only refresh.
#[derive(Clone, Copy, Debug)]
pub(super) struct SceneChanges {
    pub geometry: bool,
    pub appearance: bool,
    pub instances: bool,
}

impl SceneChanges {
    pub(super) fn reset_history(self) -> bool { self.geometry || self.appearance }
    pub(super) fn refresh_emissive(self) -> bool { self.geometry || self.appearance }
    pub(super) fn update_instances(self) -> bool { self.instances }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revision(topology: u64, positions: u64, content: u64) -> MeshRevision {
        MeshRevision { topology, positions, content }
    }

    fn slot_hint(slot: u32, generation: u64) -> Option<ContentVersion> {
        Some(ContentVersion::new(1, crate::node_graph::ResourceId(slot), generation))
    }

    #[test]
    fn appearance_tracks_content_identity_and_lifetime() {
        let key = |epoch, resource, revision| {
            let mut key = AppearanceKeyBuilder::default();
            key.content(Some(ContentVersion::new(epoch, crate::node_graph::ResourceId(resource), revision)));
            key.parameter_bytes(&1.0_f32.to_bits().to_ne_bytes());
            key.finish()
        };
        // Physical allocation/write counters are deliberately absent from this API.
        assert_eq!(key(1, 2, 3), key(1, 2, 3));
        assert_ne!(key(1, 2, 3), key(1, 2, 4));
        assert_ne!(key(1, 2, 3), key(1, 4, 3));
        assert_ne!(key(1, 2, 3), key(2, 2, 3));
    }

    #[test]
    fn change_classification_preserves_transform_reprojection_policy() {
        for geometry in [false, true] {
            for appearance in [false, true] {
                for instances in [false, true] {
                    let changes = SceneChanges { geometry, appearance, instances };
                    assert_eq!(changes.reset_history(), geometry || appearance);
                    assert_eq!(changes.refresh_emissive(), geometry || appearance);
                    assert_eq!(changes.update_instances(), instances);
                }
            }
        }
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
