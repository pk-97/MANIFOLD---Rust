use std::{env, path::PathBuf};

#[path = "../../scripts/native_source_identity.rs"]
mod native_source_identity;

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    native_source_identity::emit_source_identity(
        &root,
        &[
            "src/node_graph/fluid.rs",
            "src/node_graph/fluid",
            "src/node_graph/physics.rs",
            "src/node_graph/physics",
            "src/node_graph/physics_events.rs",
            "src/node_graph/transform.rs",
        ],
        "MANIFOLD_PHYSICS_INTEGRATION_IDENTITY",
    )
    .expect("compute physics integration source identity");
}
