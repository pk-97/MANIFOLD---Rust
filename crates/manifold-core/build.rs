use std::{env, path::PathBuf};

#[path = "../../scripts/native_source_identity.rs"]
mod native_source_identity;

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    native_source_identity::emit_source_identity(
        &root,
        &["src/tempo.rs"],
        "MANIFOLD_CORE_SOURCE_IDENTITY",
    )
    .expect("compute core tempo source identity");
}
