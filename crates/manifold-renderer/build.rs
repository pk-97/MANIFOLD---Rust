use std::{env, path::PathBuf};

#[path = "../../scripts/native_source_identity.rs"]
#[expect(dead_code, reason = "The shared helper also exposes the legacy emitter used by physics crates; this crate hashes only owned sources.")]
mod native_source_identity;

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    native_source_identity::emit_owned_source_identity(
        &root,
        &["src/node_graph/gltf_anim_identity.rs"],
        "MANIFOLD_PHYSICS_FAMILY_IDENTITY",
    )
    .expect("compute physics family source identity");
}
