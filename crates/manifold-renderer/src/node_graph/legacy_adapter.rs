//! Inventory-backed `EffectMetadata` lookup.
//!
//! Originally this module also hosted `LegacyPostProcessNode`, the
//! adapter that wrapped a `PostProcessEffect` so the chain graph could
//! treat the legacy effect catalog as graph nodes. That adapter was
//! deleted once every effect migrated to a `ChainSpec` (every chain
//! splice is direct now — no wrappers). What survived is the
//! [`metadata_by_id`] helper: a cached inventory scan that resolves an
//! `EffectTypeId` to its `&'static EffectMetadata`. Used by ChainSpec
//! routing resolution, the snapshot path, and a handful of legacy-
//! `effect_graphs.rs` entry points that haven't been retired yet.
//!
//! The file keeps its `legacy_adapter` name for now because several
//! callers still `use crate::node_graph::legacy_adapter::metadata_by_id`;
//! renaming is mechanical cleanup for a follow-up.

use std::sync::OnceLock;

use manifold_core::effect_registration::EffectMetadata;

/// Lookup a registered `EffectMetadata` by its `EffectTypeId`.
/// `inventory` collection is one-shot at startup; cache the lookup so
/// per-frame and per-chain-rebuild callers don't rescan the iterator.
pub fn metadata_by_id(id: &manifold_core::EffectTypeId) -> Option<&'static EffectMetadata> {
    static MAP: OnceLock<ahash::AHashMap<manifold_core::EffectTypeId, &'static EffectMetadata>> =
        OnceLock::new();
    let map = MAP.get_or_init(|| {
        let mut m: ahash::AHashMap<manifold_core::EffectTypeId, &'static EffectMetadata> =
            ahash::AHashMap::default();
        for meta in inventory::iter::<EffectMetadata> {
            m.insert(meta.id.clone(), meta);
        }
        m
    });
    map.get(id).copied()
}
