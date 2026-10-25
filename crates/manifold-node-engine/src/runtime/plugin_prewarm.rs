//! Plugin warmup — startup-time initialization for effects backed by
//! native plugins or background workers.
//!
//! Two of the shipping effects (`DepthOfField`, `WireframeDepth`)
//! drive native FFI plugins on background threads. (BlobTracking
//! was decomposed — its worker lives in `node.blob_tracker`.)
//! Those workers must be running before the first frame renders so
//! the first chain dispatch doesn't block on plugin initialisation.
//!
//! Each plugin-using effect submits one [`PluginPrewarm`] entry;
//! [`prewarm_all`] iterates the inventory once at startup, returning
//! the constructed processors. [`LayerCompositor`] holds the result
//! for the process lifetime so workers stay alive, and forwards
//! `resize` / `flush_background_work` through them.

use crate::runtime::effect::PostProcessEffect;
use manifold_core::PresetTypeId;
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
///         id: PresetTypeId::BLOB_TRACKING,
///         prewarm: |device| Box::new(BlobTrackingFX::new(device)),
///     }
/// }
/// ```
pub struct PluginPrewarm {
    pub id: PresetTypeId,
    pub prewarm: fn(&GpuDevice) -> Box<dyn PostProcessEffect>,
}

inventory::collect!(PluginPrewarm);

/// Runtime preparation sites supported by family pipeline providers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipelinePrewarmPurpose {
    MathView,
}

/// Family pipeline preparation, invoked at the owning runtime's preparation site.
pub struct PipelinePrewarm {
    pub purpose: PipelinePrewarmPurpose,
    pub run: fn(&GpuDevice),
}

inventory::collect!(PipelinePrewarm);

pub fn prewarm_pipeline(purpose: PipelinePrewarmPurpose, device: &GpuDevice) {
    let mut entries = inventory::iter::<PipelinePrewarm>
        .into_iter()
        .filter(|entry| entry.purpose == purpose);
    let entry = entries
        .next()
        .expect("pipeline prewarm provider must be linked");
    assert!(
        entries.next().is_none(),
        "duplicate pipeline prewarm provider"
    );
    (entry.run)(device);
}

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
pub fn prewarm_ids() -> impl Iterator<Item = &'static PresetTypeId> {
    inventory::iter::<PluginPrewarm>
        .into_iter()
        .map(|entry| &entry.id)
}
