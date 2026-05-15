//! Inventory-backed `EffectMetadata` lookup.
//!
//! Resolves an `EffectTypeId` to its `&'static EffectMetadata`. Used
//! by ChainSpec routing resolution, the snapshot path, and the editor
//! inspector. The `inventory` collection is one-shot at startup;
//! [`metadata_by_id`] caches the resulting map so per-frame callers
//! don't rescan.

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
