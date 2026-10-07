//! Registered family migrations around the loader's group flattening boundary.

use std::borrow::Cow;
use manifold_core::effect_graph_def::EffectGraphDef;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MigrationStage {
    BeforeFlatten,
    AfterFlatten,
}

pub(crate) struct GraphMigration {
    pub name: &'static str,
    pub stage: MigrationStage,
    pub order: u16,
    pub apply: fn(&mut EffectGraphDef) -> bool,
}

inventory::collect!(GraphMigration);

pub(crate) fn run_stage(def: &EffectGraphDef, stage: MigrationStage) -> Cow<'_, EffectGraphDef> {
    let mut migrations: Vec<_> = inventory::iter::<GraphMigration>
        .into_iter().filter(|migration| migration.stage == stage).collect();
    migrations.sort_by_key(|migration| (migration.order, migration.name));
    let mut current = Cow::Borrowed(def);
    for migration in migrations {
        let mut candidate = current.as_ref().clone();
        if (migration.apply)(&mut candidate) {
            current = Cow::Owned(candidate);
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_order_matches_table() {
        let mut resolved = Vec::new();
        for stage in [MigrationStage::BeforeFlatten, MigrationStage::AfterFlatten] {
            let mut migrations: Vec<_> = inventory::iter::<GraphMigration>
                .into_iter().filter(|migration| migration.stage == stage).collect();
            migrations.sort_by_key(|migration| (migration.order, migration.name));
            resolved.extend(migrations.into_iter().map(|migration| (stage, migration.order, migration.name)));
        }
        use MigrationStage::{BeforeFlatten, AfterFlatten};
        assert_eq!(resolved, [
            (BeforeFlatten, 200, "migrate_gltf_anim_v2"),
            (BeforeFlatten, 210, "migrate_gltf_ao_mask"),
            (AfterFlatten, 300, "wire_liquid_intervals"),
            (AfterFlatten, 310, "wire_gpu_flip_grid"),
            (AfterFlatten, 320, "wire_liquid_frame_cursor"),
            (AfterFlatten, 330, "wire_retained_whitewater"),
            (AfterFlatten, 400, "wire_blob_bounds"),
        ]);
    }
}
