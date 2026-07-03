//! Load-time rename table for node/preset `type_id` strings — the
//! infrastructure half of `docs/NODE_VOCABULARY_AUDIT.md`.
//!
//! One flat string→string table serves every id namespace that identifies a
//! node or preset by name: graph-node `type_id`s (`node.gain`) and
//! [`crate::PresetTypeId`] values (`"Bloom"`, `"EdgeGlow"`). The two
//! namespaces never collide (`node.`/`system.` prefix vs. bare PascalCase),
//! so one table and one lookup function cover both — matching §3's "one
//! static table" shape rather than one per document kind.
//!
//! **Old ids are never reused** (§2 rule 5): once an id is retired here it is
//! retired permanently, so a stale entry can never resolve to the wrong
//! current node.
//!
//! **What this table is NOT for:** the bundled preset JSON library, parity/
//! gpu tests, `hand_descriptor!` entries, and `primitive!` registrations are
//! rewritten directly in the repo as part of the rename commit (§3, fourth
//! bullet) — they never carry an old id in the first place, so migration
//! never runs on them. This table exists only for **content that already
//! shipped**: saved project files (clips, effect/generator instances, their
//! embedded [`crate::effect_graph_def::EffectGraphDef`] graphs) that may
//! still carry an id from before a rename.
//!
//! Choke points (both wired in P1 — see `docs/NODE_VOCABULARY_AUDIT.md` §3,
//! §9 P1):
//! - `EffectGraphDef` node `type_id`s — migrated in
//!   `manifold_renderer::node_graph::graph_loader::instantiate_def`, before
//!   the group flatten, recursing into group bodies. Every loader (generator
//!   load, effect splice, freeze/proof harnesses) converges on
//!   `instantiate_def`, so this is the single place a graph gets built from a
//!   def.
//! - [`crate::PresetTypeId`] on clips (`generator_type`) and effect/generator
//!   instances — migrated inside `preset_type_id`'s deserializers
//!   (`deserialize_effect_type`, `deserialize_generator_type`, and the plain
//!   `Deserialize` impl), chained after the existing `remap_legacy_string`
//!   step. That module already had exactly this choke point for one
//!   hardcoded legacy rename (`BasicShapesSnap` → `BasicShapes`); this table
//!   generalizes it rather than adding a second mechanism.

use crate::effect_graph_def::SerializedParamValue;

/// The real rename table. **Empty in every shipped build** — P1 lands the
/// infrastructure only; P2/P3 populate real entries one rename-commit at a
/// time. The one entry below is a fixture id, not a real node/preset — it
/// exists so cross-crate tests (`manifold-core`, `manifold-renderer`,
/// `manifold-io`) can exercise every choke point without depending on a
/// `#[cfg(test)]` item from a dependency, which wouldn't compile in when this
/// crate is built as a normal (non-test) library dependency of another
/// crate's test binary.
pub static TYPE_ID_MIGRATIONS: &[(&str, &str)] = &[
    (
        "__vocab_migration_test_old__",
        "__vocab_migration_test_new__",
    ),
    // --- VOCAB P2 1/8: Color & Tone / Composite (docs/NODE_VOCABULARY_AUDIT.md §4) ---
    ("node.gain", "node.exposure"),
    ("node.color_ramp", "node.gradient_map"),
    ("node.channel_mix", "node.channel_mixer"),
    ("node.clamp_texture", "node.clamp"),
    ("node.hdr_retention_mix", "node.hdr_mix"),
];

/// One legacy-fold entry: `(old_id, new_id, seed_params)` — the params to
/// write onto the successor node so it reproduces the retired node's fixed
/// behavior (e.g. `angle = 90` for `node.rotate_vec2_90` folding into
/// `node.rotate_vector`).
pub type ParamSeedMigration = (&'static str, &'static str, &'static [(&'static str, SerializedParamValue)]);

/// Param-seeding table for §7 legacy folds: a retired node (`old_id`) with no
/// direct id-for-id equivalent folds into a parameterized successor
/// (`new_id`), seeding the params a plain rename can't express. Empty until
/// P4 — folding is content work, not infrastructure, and each fold needs its
/// §7 port-parity verification run first.
pub static PARAM_SEED_MIGRATIONS: &[ParamSeedMigration] = &[];

/// Map an old `type_id`/[`crate::PresetTypeId`] string to its current name.
/// Identity for any id not in [`TYPE_ID_MIGRATIONS`] — covers every current
/// id (the overwhelming majority) and any id this table doesn't know about.
pub fn migrate_type_id(id: &str) -> &str {
    TYPE_ID_MIGRATIONS
        .iter()
        .find(|(old, _)| *old == id)
        .map(|(_, new)| *new)
        .unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_id_is_identity() {
        // `node.mix` and `Bloom` are never renamed by the vocabulary audit
        // (docs/NODE_VOCABULARY_AUDIT.md §4/§6) — safe stand-ins for "any
        // current id with no migration entry".
        assert_eq!(migrate_type_id("node.mix"), "node.mix");
        assert_eq!(migrate_type_id("Bloom"), "Bloom");
    }

    #[test]
    fn fixture_entry_migrates() {
        assert_eq!(
            migrate_type_id("__vocab_migration_test_old__"),
            "__vocab_migration_test_new__"
        );
    }

    #[test]
    fn empty_string_is_identity() {
        assert_eq!(migrate_type_id(""), "");
    }
}
