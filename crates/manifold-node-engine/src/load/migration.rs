//! Registered family transformations at explicit graph preparation boundaries.

use std::borrow::Cow;
use manifold_core::effect_graph_def::EffectGraphDef;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationStage {
    BeforeFlatten,
    AfterFlatten,
    /// Runtime render copy, before scene-modifier expansion. Never saved.
    BeforeSceneModifiers,
    /// Flattened render copy, before bindings and the runtime graph are built.
    BeforeBindingCapture,
}

pub struct GraphMigration {
    pub name: &'static str,
    pub stage: MigrationStage,
    pub order: u16,
    pub apply: fn(&mut EffectGraphDef) -> bool,
}

inventory::collect!(GraphMigration);

fn ordered(stage: MigrationStage) -> Vec<&'static GraphMigration> {
    let mut migrations: Vec<_> = inventory::iter::<GraphMigration>
        .into_iter().filter(|migration| migration.stage == stage).collect();
    migrations.sort_by_key(|migration| (migration.order, migration.name));
    migrations
}

/// Prepare an owned runtime document in place; all callbacks run even after
/// one changes it. The caller retains the authored document separately.
pub(crate) fn prepare_stage(def: &mut EffectGraphDef, stage: MigrationStage) -> bool {
    let mut changed = false;
    for migration in ordered(stage) {
        changed |= (migration.apply)(def);
    }
    changed
}

pub(crate) fn run_stage(def: &EffectGraphDef, stage: MigrationStage) -> Cow<'_, EffectGraphDef> {
    let mut current = Cow::Borrowed(def);
    for migration in ordered(stage) {
        let mut candidate = current.as_ref().clone();
        if (migration.apply)(&mut candidate) {
            current = Cow::Owned(candidate);
        }
    }
    current
}
