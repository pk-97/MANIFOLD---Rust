//! Transactional, load-only adoption of complete legacy scene modifier graphs
//! and conservative repair of known stock fragment-mask snapshots.
//! Authored custom graphs remain intact when their ownership is ambiguous.

mod fragment_masks;
mod loop_upgrade;
mod photoscan;
mod sources;

use fragment_masks::migrate_stock_fragment_masks;
use manifold_core::effect_graph_def::EffectGraphDef;

use super::{PrimitiveRegistry, scene_modifier_expand::prepare_scene_modifiers};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SceneModifierMigrationReport {
    pub changed: bool,
    pub diagnostics: Vec<String>,
}

/// Extract into a private candidate, admit through canonical preparation, then
/// publish once. A partial conversion is never exposed to playback or saving.
pub fn migrate_legacy_scene_modifiers(
    def: &mut EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> SceneModifierMigrationReport {
    if !def.scene_modifiers.is_empty() {
        return SceneModifierMigrationReport {
            changed: migrate_stock_fragment_masks(def),
            diagnostics: Vec::new(),
        };
    }
    let result = (|| -> Result<Option<EffectGraphDef>, String> {
        // Historical fixed-row arithmetic stays inside the same private
        // adoption transaction. Incomplete takeover history is never guessed.
        let mut upgraded = def.clone();
        loop_upgrade::upgrade_known_loop(&mut upgraded)?;
        let photoscan = photoscan::extract(&upgraded)?;
        let candidate = photoscan.as_ref().unwrap_or(&upgraded);
        let sources = sources::extract(candidate, registry)?;
        let Some(candidate) = sources.or(photoscan) else {
            return Ok(None);
        };
        let mut candidate = candidate;
        migrate_stock_fragment_masks(&mut candidate);
        prepare_scene_modifiers(&candidate, registry).map_err(|error| error.to_string())?;
        Ok(Some(candidate))
    })();
    match result {
        Ok(Some(candidate)) => {
            *def = candidate;
            SceneModifierMigrationReport {
                changed: true,
                diagnostics: Vec::new(),
            }
        }
        Ok(None) => SceneModifierMigrationReport::default(),
        Err(reason) => SceneModifierMigrationReport {
            changed: false,
            diagnostics: vec![format!(
                "Legacy scene modifiers remain an ordinary editable graph: {reason}"
            )],
        },
    }
}
