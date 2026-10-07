use std::{env, path::PathBuf};

#[path = "../../scripts/native_source_identity.rs"]
mod native_source_identity;

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    native_source_identity::emit_source_identity(
        &root,
        &[
            "src/water/fluid.rs",
            "src/water/fluid",
            "src/water/physics.rs",
            "src/water/physics",
            "src/water/physics_events.rs",
            "src/scene/source_asset.rs",
            "src/scene/transform.rs",
            "src/water/runtime/physics_sampling.rs",
            "src/water/runtime/physics_sources.rs",
            "src/water/runtime/physics_source_runtime.rs",
            "src/water/runtime/physics_source_controls.rs",
            "src/water/runtime/physics_source_state.rs",
            "src/water/runtime/physics_source_chain.rs",
            "src/runtime/preset_context.rs",
        ],
        "MANIFOLD_PHYSICS_INTEGRATION_IDENTITY",
    )
    .expect("compute physics integration source identity");
}
