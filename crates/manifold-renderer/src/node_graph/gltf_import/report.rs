//! Import report — what the glTF assembler did, for the caller to report on.

/// What the assembler did, for the caller (importer UI, tests) to report
/// or warn on. Not part of the graph itself.
///
/// `Serialize` (runtime-only type — never persisted into `.manifold`) backs
/// the P3-D INV-R8 equivalence harness: `render-import --dump-def` serializes
/// `(EffectGraphDef, ImportReport)` so a table-ization change can be proven
/// byte-identical against a pre-change capture.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportReport {
    /// Distinct materials with geometry, as parsed. Import is 1:1
    /// (GLB_CONFORMANCE_DESIGN.md D4) — always equal to `object_count`.
    pub material_count: usize,
    /// Objects wired into `node.render_scene` — always equal to
    /// `material_count`; nothing is ever dropped for exceeding a count
    /// (`assemble_import_graph` errors instead, see [`OBJECT_SAFETY_MAX`]).
    pub object_count: usize,
    /// How many objects got a `node.gltf_texture_source` → `base_color_map_N` wire.
    pub textures_wired: usize,
    /// Triangle-list vertices belonging to glTF's unassigned default
    /// material — imported as a real object since BUG-171 (mirrors
    /// [`gltf_load::GltfImportSummary::default_material_vertex_count`]).
    pub default_material_vertex_count: u32,
    /// Always `true` today — the assembler always synthesizes a framing
    /// orbit camera and it is always the default active camera, even when
    /// the glb also carries embedded cameras (BUG-d2qz: those import as
    /// extra selectable `node.free_camera` cards, reported via
    /// `report_lines`, never as a replacement for this default). Kept as a
    /// field so a future "default to an authored camera" mode has
    /// somewhere to report `false`.
    pub camera_synthesized: bool,
    /// D9 doctrine ("every import produces a report") applied to the
    /// per-material features F-P4 parses but cannot yet map: clearcoat
    /// (Deferred #1), transmission (report-only until F-P5), and BLEND
    /// materials downgraded to Mask cutout (the F-P5 stopgap). One line per
    /// occurrence, naming the material. Never silently dropped.
    pub report_lines: Vec<String>,
}
