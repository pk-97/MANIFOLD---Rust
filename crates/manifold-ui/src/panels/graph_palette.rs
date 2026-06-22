//! Graph-editor palette data.
//!
//! The left-sidebar `GraphPalette` panel was retired — the node list moved to
//! the editor's node-spawn popup (`BrowserPopupPanel`), driven by an
//! `OpenNodePicker` → `AddGraphNodeAt` flow. Only the data carrier survives:
//! [`GraphPaletteAtom`], the app's cache of the catalog's spawnable atoms,
//! consumed by the spawn popup.

/// One palette entry the user can spawn as a node. Owned strings so the list
/// can be cloned across the content/UI boundary without borrowing back into
/// the renderer's `&'static` symbol table.
#[derive(Debug, Clone)]
pub struct GraphPaletteAtom {
    /// Display label, e.g. "Blur", "Mix".
    pub label: String,
    /// Stable `type_id` for the node — passed verbatim to
    /// `AddGraphNodeCommand`.
    pub type_id: String,
    /// Stable section identifier. UI groups entries with the same
    /// `category` under one header. Matches `PaletteCategory::label()`
    /// on the renderer side. Plain string here so this crate stays
    /// independent of `manifold-renderer`.
    pub category: String,
}
