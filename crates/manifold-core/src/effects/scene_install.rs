//! How a scene layer's compiled graph reaches its generator instance
//! (SCENE_DOCUMENT_DESIGN.md section 7.3 (The compiler)). The graph lands
//! the way every graph replacement lands: in `graph`, with the structure
//! version bumped so the renderer rebuilds the chain on its next frame, and
//! the manifest rebuilt from the graph with every value kept by id. A param
//! write never comes here.

use super::*;
use crate::scene_doc::SceneDoc;

impl PresetInstance {
    /// The graph a save writes. A scene layer saves its document instead; its
    /// graph is compiled, never saved.
    pub(super) fn saved_graph(&self) -> Option<&EffectGraphDef> {
        if self.scene.is_some() { None } else { self.graph.as_ref() }
    }

    /// Set this generator's scene document and install the graph compiled
    /// from it: the one install path for a structural edit, a new scene and
    /// its undo. A document that compiles to the graph already installed (a
    /// rename) leaves the graph, its version and the manifest alone, so the
    /// runtime keeps every node's state. On a compile error nothing changes.
    pub fn install_scene_document(&mut self, doc: SceneDoc) -> Result<(), String> {
        let graph = crate::scene_doc::compile(&doc)?;
        self.scene = Some(doc);
        if self.graph.as_ref() != Some(&graph) {
            self.graph = Some(graph);
            self.bump_graph_structure_version();
            self.refresh_manifest_from_graph();
        }
        Ok(())
    }

    /// Load: compile a scene layer's document so its saved values reconcile
    /// against the compiled table. A document that fails to compile leaves
    /// the layer without a graph, and the load report counts it. Whether it
    /// compiled now.
    pub(super) fn compile_scene_for_load(&mut self) -> bool {
        let Some(doc) = &self.scene else {
            return false;
        };
        if self.graph.is_some() {
            return false;
        }
        match crate::scene_doc::compile(doc) {
            Ok(graph) => {
                self.graph = Some(graph);
                true
            }
            Err(error) => {
                eprintln!("[manifold-core] a scene layer's document did not compile: {error}");
                false
            }
        }
    }
}
