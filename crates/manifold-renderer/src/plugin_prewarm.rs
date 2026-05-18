//! Plugin warmup — startup-time initialization for effects backed by
//! native plugins or background workers.
//!
//! Three of the shipping effects (`BlobTracking`, `DepthOfField`,
//! `WireframeDepth`) drive native FFI plugins on background threads.
//! Those workers must be running before the first frame renders so
//! the first chain dispatch doesn't block on plugin initialisation.
//!
//! Today (pre-§11-cutover): worker init happens as a side effect of
//! [`EffectRegistry::new`](crate::EffectRegistry::new) constructing
//! every legacy `Box<dyn PostProcessEffect>` singleton.
//!
//! Post-§11 (when [`EffectRegistry`] and [`EffectFactory`] are
//! deleted in block 8): worker init flows through this channel.
//! Each plugin-using primitive submits one [`PluginPrewarm`] entry;
//! [`prewarm_all`] iterates the inventory once at startup, holding
//! the returned handles in a process-wide static so the workers stay
//! alive for the process lifetime.
//!
//! This module defines the channel + submissions today. The
//! consumer (`prewarm_all` invocation from app startup) wires up in
//! block 8 alongside the [`EffectRegistry`] deletion. Until then it's
//! pure infrastructure — parallel to but unused by the existing
//! warmup path.

use manifold_core::EffectTypeId;
use manifold_gpu::GpuDevice;

/// One plugin-using effect's warmup contribution.
///
/// `prewarm` is a function pointer (const-compatible) that the
/// renderer invokes once at startup with the live [`GpuDevice`]. The
/// returned `Box<dyn Any + Send + Sync>` carries any state the
/// warmup created — typically the constructed legacy effect or a
/// worker handle — and is held by the renderer's process-wide
/// warmup store for the process lifetime so background workers stay
/// alive.
///
/// Submitted via `inventory::submit!`:
///
/// ```ignore
/// inventory::submit! {
///     PluginPrewarm {
///         id: EffectTypeId::BLOB_TRACKING,
///         prewarm: prewarm_blob_tracking,
///     }
/// }
///
/// fn prewarm_blob_tracking(device: &GpuDevice) -> Box<dyn Any + Send + Sync> {
///     Box::new(BlobTrackingFX::new(device))
/// }
/// ```
pub struct PluginPrewarm {
    pub id: EffectTypeId,
    pub prewarm: fn(&GpuDevice) -> Box<dyn std::any::Any + Send>,
}

inventory::collect!(PluginPrewarm);

/// Run every registered [`PluginPrewarm`] submission, returning the
/// vector of opaque state handles. The caller (renderer's compositor)
/// must hold the returned `Vec` for the process lifetime so the
/// background workers stay alive.
///
/// Currently unused — the existing [`EffectRegistry`] handles this
/// path. Block 8 of the §11 migration switches the renderer's
/// compositor to call this at startup and delete `EffectRegistry`.
#[must_use = "the returned Vec holds worker state; drop it and workers die"]
pub fn prewarm_all(device: &GpuDevice) -> Vec<Box<dyn std::any::Any + Send>> {
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

    /// The three plugin-using effects (per the §11 audit) must each
    /// have a registered prewarm. Adding a new plugin-using effect =
    /// add an `inventory::submit!(PluginPrewarm { ... })` in its file;
    /// this test catches forgetting it.
    #[test]
    fn plugin_using_effects_all_have_prewarm_submissions() {
        let registered: std::collections::HashSet<EffectTypeId> =
            prewarm_ids().cloned().collect();
        let expected = [
            EffectTypeId::BLOB_TRACKING,
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
