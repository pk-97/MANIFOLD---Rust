use std::{env, path::PathBuf};

#[path = "../../scripts/native_source_identity.rs"]
#[expect(dead_code, reason = "The shared helper also exposes the legacy emitter used by physics crates; this crate hashes only owned sources.")]
mod native_source_identity;

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    native_source_identity::emit_owned_source_identity(
        &root,
        &[
            "src/fluid.rs",
            "src/fluid",
            "src/physics.rs",
            "src/physics",
            "src/physics_events.rs",
            "src/runtime/physics_sampling.rs",
            "src/runtime/physics_sources.rs",
            "src/runtime/physics_source_runtime.rs",
            "src/runtime/physics_source_controls.rs",
            "src/runtime/physics_source_state.rs",
            "src/runtime/physics_source_chain.rs",
        ],
        "MANIFOLD_WATER_SOURCE_IDENTITY",
    )
    .expect("compute water source identity");
}
