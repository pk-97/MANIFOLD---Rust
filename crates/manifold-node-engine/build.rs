use std::{env, path::PathBuf};

#[path = "../../scripts/native_source_identity.rs"]
#[expect(dead_code, reason = "The shared helper also exposes the legacy emitter used by physics crates; this crate hashes only owned sources.")]
mod native_source_identity;

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    native_source_identity::emit_owned_source_identity(
        &root,
        &[
            "src/scene/source_asset.rs",
            "src/scene/transform.rs",
            "src/runtime/preset_context.rs",
        ],
        "MANIFOLD_PHYSICS_INTEGRATION_IDENTITY",
    )
    .expect("compute engine source identity");
    native_source_identity::emit_owned_source_identity(
        &root,
        &[
            "src/water/fluid.rs",
            "src/water/fluid",
            "src/water/physics.rs",
            "src/water/physics",
            "src/water/physics_events.rs",
            "src/water/runtime/physics_sampling.rs",
            "src/water/runtime/physics_sources.rs",
            "src/water/runtime/physics_source_runtime.rs",
            "src/water/runtime/physics_source_controls.rs",
            "src/water/runtime/physics_source_state.rs",
            "src/water/runtime/physics_source_chain.rs",
        ],
        "MANIFOLD_WATER_SOURCE_IDENTITY",
    )
    .expect("compute water source identity");
}
