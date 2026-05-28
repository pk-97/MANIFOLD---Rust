//! Plugin warmup — startup-time initialization for effects backed by
//! native plugins or background workers.
//!
//! Two of the shipping effects (`DepthOfField`, `WireframeDepth`)
//! drive native FFI plugins on background threads. (BlobTracking
//! was decomposed — its worker lives in `node.blob_detect_ffi`.)
//! Those workers must be running before the first frame renders so
//! the first chain dispatch doesn't block on plugin initialisation.
//!
//! Each plugin-using effect submits one [`PluginPrewarm`] entry;
//! [`prewarm_all`] iterates the inventory once at startup, returning
//! the constructed processors. [`LayerCompositor`] holds the result
//! for the process lifetime so workers stay alive, and forwards
//! `resize` / `flush_background_work` through them.

use crate::effect::PostProcessEffect;
use manifold_core::EffectTypeId;
use manifold_gpu::GpuDevice;

/// One plugin-using effect's warmup contribution.
///
/// `prewarm` constructs a [`PostProcessEffect`] (today, the legacy
/// effect struct that owns the background worker handle). The
/// renderer holds the returned `Box<dyn PostProcessEffect>` for the
/// process lifetime and dispatches `resize` / `flush_background_work`
/// to it; the trait's `apply` method is never called on prewarm
/// processors — chain dispatch goes through the primitive registry,
/// not these handles.
///
/// Submitted via `inventory::submit!`:
///
/// ```ignore
/// inventory::submit! {
///     PluginPrewarm {
///         id: EffectTypeId::BLOB_TRACKING,
///         prewarm: |device| Box::new(BlobTrackingFX::new(device)),
///     }
/// }
/// ```
pub struct PluginPrewarm {
    pub id: EffectTypeId,
    pub prewarm: fn(&GpuDevice) -> Box<dyn PostProcessEffect>,
}

inventory::collect!(PluginPrewarm);

/// Run every registered [`PluginPrewarm`] submission, returning the
/// vector of constructed processors. The caller (renderer's
/// compositor) must hold the returned `Vec` for the process lifetime
/// so the background workers stay alive.
#[must_use = "the returned Vec holds worker state; drop it and workers die"]
pub fn prewarm_all(device: &GpuDevice) -> Vec<Box<dyn PostProcessEffect>> {
    inventory::iter::<PluginPrewarm>
        .into_iter()
        .map(|entry| (entry.prewarm)(device))
        .collect()
}

/// Every effect type id with a registered prewarm submission. Useful
/// for tests asserting coverage.
pub fn prewarm_ids() -> impl Iterator<Item = &'static EffectTypeId> {
    inventory::iter::<PluginPrewarm>
        .into_iter()
        .map(|entry| &entry.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plugin-using effects must each have a registered prewarm.
    /// Adding a new plugin-using effect = add an
    /// `inventory::submit!(PluginPrewarm { ... })` in its file;
    /// this test catches forgetting it. (BlobTracking decomposed —
    /// its worker is in node.blob_detect_ffi, no prewarm needed.)
    #[test]
    fn plugin_using_effects_all_have_prewarm_submissions() {
        let registered: std::collections::HashSet<EffectTypeId> =
            prewarm_ids().cloned().collect();
        let expected = [
            EffectTypeId::DEPTH_OF_FIELD,
            EffectTypeId::WIREFRAME_DEPTH,
        ];
        for id in expected {
            assert!(
                registered.contains(&id),
                "{} must have an inventory::submit!(PluginPrewarm) — \
                 background workers need warmup at startup",
                id.as_str()
            );
        }
    }
}
