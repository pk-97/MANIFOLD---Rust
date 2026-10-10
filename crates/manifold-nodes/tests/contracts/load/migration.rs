use manifold_node_engine::load::migration::{GraphMigration, MigrationStage};

mod tests {
use crate::contracts::load::migration::*;

#[test]
fn migration_order_matches_table() {
    let mut resolved = Vec::new();
    use MigrationStage::{AfterFlatten, BeforeBindingCapture, BeforeFlatten, BeforeSceneModifiers};
    for stage in [BeforeFlatten, AfterFlatten, BeforeSceneModifiers, BeforeBindingCapture] {
        let mut migrations: Vec<_> = inventory::iter::<GraphMigration>
            .into_iter()
            .filter(|migration| migration.stage == stage)
            .collect();
        migrations.sort_by_key(|migration| (migration.order, migration.name));
        resolved.extend(
            migrations
                .into_iter()
                .map(|migration| (stage, migration.order, migration.name)),
        );
    }
    assert_eq!(
        resolved,
        [
            (BeforeFlatten, 200, "migrate_gltf_anim_v2"),
            (BeforeFlatten, 210, "migrate_gltf_ao_mask"),
            (AfterFlatten, 300, "wire_liquid_intervals"),
            (AfterFlatten, 310, "wire_gpu_flip_grid"),
            (AfterFlatten, 320, "wire_liquid_frame_cursor"),
            (AfterFlatten, 330, "wire_retained_whitewater"),
            (AfterFlatten, 400, "wire_blob_bounds"),
            (BeforeSceneModifiers, 300, "prepare_gpu_flip_surface"),
            (BeforeBindingCapture, 310, "wire_gpu_flip_grid"),
        ]
    );
}
}
