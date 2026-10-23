//! How a scene layer's compiled graph reaches its generator instance
//! (SCENE_DOCUMENT_DESIGN.md section 7.3 (The compiler)). The compiled scene
//! goes into the layer graph's sealed Scene node (`crate::scene_node`), and
//! everything the user wired around that node stays. The graph lands the way
//! every graph replacement lands: in `graph`, with the structure version
//! bumped so the renderer rebuilds the chain on its next frame, and the
//! manifest rebuilt from the graph with every value kept by id. A param write
//! never comes here.

use super::*;
use crate::scene_doc::SceneDoc;

impl PresetInstance {
    /// The graph the saved params are described against. A scene layer's
    /// params are described by its kind tables, never by a graph.
    pub(super) fn saved_graph(&self) -> Option<&EffectGraphDef> {
        if self.scene.is_some() { None } else { self.graph.as_ref() }
    }

    /// A scene layer's graph as a save writes it: the user's nodes and wires,
    /// with the Scene node emptied (`crate::scene_node::saved`).
    pub(super) fn saved_scene_graph(&self) -> Option<EffectGraphDef> {
        self.scene.as_ref()?;
        self.graph.as_ref().map(crate::scene_node::saved)
    }

    /// Set this generator's scene document and install the graph compiled
    /// from it: the one install path for a structural edit, a new scene and
    /// its undo. Only the Scene node changes; the nodes wired around it stay.
    /// A document that compiles to the graph already installed (a rename)
    /// leaves the graph, its version and the manifest alone, so the runtime
    /// keeps every node's state. On a compile error nothing changes.
    pub fn install_scene_document(&mut self, doc: SceneDoc) -> Result<(), String> {
        let graph = crate::scene_node::seal(crate::scene_doc::compile(&doc)?, self.graph.as_ref())?;
        self.scene = Some(doc);
        if self.graph.as_ref() != Some(&graph) {
            self.graph = Some(graph);
            self.bump_graph_structure_version();
            self.refresh_manifest_from_graph();
        }
        Ok(())
    }

    /// Whether this scene layer's Scene node holds its compiled scene. A
    /// loaded layer holds an empty one until [`Self::compile_scene_for_load`].
    pub(super) fn scene_compiled(&self) -> bool {
        self.graph.as_ref().is_some_and(crate::scene_node::is_filled)
    }

    /// Load: compile a scene layer's document into its saved graph, so its
    /// saved values reconcile against the compiled table. A document that
    /// fails to compile leaves the Scene node empty, and the load report
    /// counts it. Whether it compiled now.
    pub(super) fn compile_scene_for_load(&mut self) -> bool {
        let Some(doc) = &self.scene else {
            return false;
        };
        if self.scene_compiled() {
            return false;
        }
        match crate::scene_doc::compile(doc).and_then(|compiled| crate::scene_node::seal(compiled, self.graph.as_ref())) {
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
