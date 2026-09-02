//! P5 exhaustiveness gate (PROJECT_FOLDERS_DESIGN section 5): the single list
//! of file-loading node types is `manifold_core::file_loader::file_loader_kind`.
//! These tests keep that list honest from the renderer side — a new
//! file-loading primitive without a table entry is a red test, not a silent
//! skip, and a table entry naming a type that isn't registered is a red test
//! too.

use manifold_core::file_loader::{ALL_FILE_LOADER_TYPE_IDS, file_loader_kind};
use manifold_renderer::node_graph::{ParamType, PrimitiveRegistry};

/// Load-bearing externs behind the "is this a file reader?" check.
///
/// A primitive is a file reader iff it declares a `ParamType::String` param
/// named `path` or `folder` — the exact convention every file-reading
/// primitive uses ("path comes via presetMetadata.stringBindings… same
/// convention as node.gltf_mesh_source's `path`" / `node.image_folder`'s
/// `folder`). String params with OTHER names (layer ids, font names, enum
/// hints) are not collected, so they shouldn't require a table entry.
fn is_file_reader(param_names: &[&str]) -> bool {
    param_names.iter().any(|n| *n == "path" || *n == "folder")
}

#[test]
fn every_registered_file_reader_has_a_core_table_entry() {
    let registry = PrimitiveRegistry::with_builtin();
    let mut readers_without_entry = Vec::new();

    for type_id in registry.known_type_ids() {
        // Skip the system boundary nodes — they have no params.
        if type_id.starts_with("system.") {
            continue;
        }
        let Some(node) = registry.construct(type_id) else {
            continue;
        };
        let string_param_names: Vec<&str> = node
            .parameters()
            .iter()
            .filter(|p| p.ty == ParamType::String)
            .map(|p| p.name.as_ref())
            .collect();
        if !is_file_reader(&string_param_names) {
            continue;
        }
        if file_loader_kind(type_id).is_none() {
            readers_without_entry.push((type_id.to_string(), string_param_names.join(",")));
        }
    }

    assert!(
        readers_without_entry.is_empty(),
        "file-reading primitives with no file_loader_kind entry: {readers_without_entry:?}"
    );
}

#[test]
fn every_table_entry_names_a_registered_primitive() {
    let registry = PrimitiveRegistry::with_builtin();
    let missing: Vec<&str> = ALL_FILE_LOADER_TYPE_IDS
        .iter()
        .copied()
        .filter(|t| !registry.contains(t))
        .collect();
    assert!(
        missing.is_empty(),
        "file_loader_kind table names unregistered primitives: {missing:?}"
    );
}

#[test]
fn table_entries_are_exactly_the_file_reading_primitives() {
    // Every table entry must resolve to a real primitive that declares the
    // path/folder string param it loads (guards against a table row describing
    // a node that doesn't actually read a file).
    let registry = PrimitiveRegistry::with_builtin();
    for type_id in ALL_FILE_LOADER_TYPE_IDS {
        let node = registry
            .construct(type_id)
            .unwrap_or_else(|| panic!("table names unregistered {type_id}"));
        let string_params: Vec<&str> = node
            .parameters()
            .iter()
            .filter(|p| p.ty == ParamType::String)
            .map(|p| p.name.as_ref())
            .collect();
        assert!(
            is_file_reader(&string_params),
            "{type_id} is in the file_loader table but declares no path/folder string param (params: {string_params:?})"
        );
    }
}