//! The migration notices the journeys assert on.

use manifold_core::project::Project;

/// Old projects get no more time (BUG-bzid9): a legacy project keeps the
/// camera-controls restore notice it now earns on every load. The journeys
/// check the other migrations, so this returns every notice except that one.
pub(super) fn migrate_notices(project: &mut Project) -> Vec<String> {
    let mut notices = crate::project_io::migrate_project_scene_graphs(project);
    notices.retain(|notice| !notice.starts_with("Scene camera controls restored"));
    notices
}
