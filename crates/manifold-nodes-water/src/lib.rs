//! The water registration crate: links the six manifold-water-* crates and
//! the gas solver (Smoke and Fire), and
//! owns what names more than one of them: graph migrations, the bundled water
//! presets, the physics scene, the runtime extension and the cross-solver
//! test harnesses. Never depends on another node family or the UI.

use manifold_gas as _;
use manifold_water_gpu_flip as _;
use manifold_water_gpu_mpm as _;
use manifold_water_liquid as _;
use manifold_water_rigid as _;
use manifold_water_surface as _;
use manifold_water_whitewater as _;

pub mod graph_install;
pub(crate) mod migration;
pub(crate) mod physics_scene;
pub mod presets;
pub mod primitives;
pub mod runtime;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;

/// Prewarm the fixed pipelines of every linked water crate.
pub fn prewarm_pipelines(device: &manifold_gpu::GpuDevice) {
    manifold_water_rigid::primitives::physics_world::PhysicsWorldNode::prewarm_pipeline(device);
}
