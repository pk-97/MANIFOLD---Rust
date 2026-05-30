//! `gen-node-catalog` — regenerate the node catalog from the live registry.
//!
//! The node registry (the `primitive!` macro + hand-written
//! `NodeDescriptor` submissions) is the single source of truth. This bin
//! derives two docs from it:
//!
//! - `docs/node_catalog.json` — the machine-readable descriptor the AI
//!   composition surface consumes.
//! - the marker-delimited "Registered node index" block in
//!   `docs/NODE_CATALOG.md`.
//!
//! Usage:
//! - `cargo run -p manifold-renderer --bin gen_node_catalog`           → write both
//! - `cargo run -p manifold-renderer --bin gen_node_catalog -- --check` → verify in sync (CI / pre-commit)
//!
//! The same drift check runs as the `catalog_gen::tests::regenerates_in_sync`
//! lib test, so a stale doc fails `cargo test` too.

use std::path::{Path, PathBuf};

use manifold_renderer::node_graph::catalog_gen;

fn docs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs")
}

fn main() {
    let check = std::env::args().any(|a| a == "--check");
    let docs = docs_dir();
    let json_path = docs.join("node_catalog.json");
    let md_path = docs.join("NODE_CATALOG.md");

    let json = catalog_gen::node_catalog_json();

    let md_existing = match std::fs::read_to_string(&md_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: read {}: {e}", md_path.display());
            std::process::exit(2);
        }
    };
    let md_new = match catalog_gen::inject(&md_existing) {
        Some(s) => s,
        None => {
            eprintln!(
                "error: {} is missing the generated-block markers. Add them once:",
                md_path.display()
            );
            eprintln!("  {}", catalog_gen::BEGIN_MARKER);
            eprintln!("  {}", catalog_gen::END_MARKER);
            std::process::exit(2);
        }
    };

    if check {
        let mut drift = false;
        match std::fs::read_to_string(&json_path) {
            Ok(on_disk) if on_disk == json => {}
            Ok(_) => {
                println!("DRIFT docs/node_catalog.json");
                drift = true;
            }
            Err(e) => {
                println!("MISSING docs/node_catalog.json ({e})");
                drift = true;
            }
        }
        if md_existing != md_new {
            println!("DRIFT docs/NODE_CATALOG.md (generated block)");
            drift = true;
        }
        if drift {
            println!(
                "\nout of sync — run `cargo run -p manifold-renderer --bin gen_node_catalog`"
            );
            std::process::exit(1);
        }
        println!("node catalog in sync");
        return;
    }

    if let Err(e) = std::fs::write(&json_path, &json) {
        eprintln!("error: write {}: {e}", json_path.display());
        std::process::exit(2);
    }
    if let Err(e) = std::fs::write(&md_path, &md_new) {
        eprintln!("error: write {}: {e}", md_path.display());
        std::process::exit(2);
    }
    println!("wrote {}", json_path.display());
    println!("wrote {} (generated block)", md_path.display());
}
