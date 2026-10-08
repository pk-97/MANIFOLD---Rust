//! Registered family migrations around the loader's group flattening boundary.

use std::borrow::Cow;
use manifold_core::effect_graph_def::EffectGraphDef;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationStage {
    BeforeFlatten,
    AfterFlatten,
}

pub struct GraphMigration {
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
