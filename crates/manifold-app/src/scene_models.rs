//! The facts of every scene layer's model files (SCENE_DOCUMENT_DESIGN.md
//! section 7.7 (Imports)): read once on a worker thread, kept here by path on
//! the UI thread, and put on each document that names the file, on both
//! threads, by one project write. No compile on the UI or content thread
//! opens a model file; one whose facts have not arrived draws as a plain
//! mesh until they do.
//!
//! An import reads its file's facts with its parts (`import_worker.rs`).
//! Every other way a document comes to name a file without them (a load, a
//! paste, an undo past the read, a relink) [`ModelStore::sweep`] finds, and
//! it looks only when a layer or an instance's structure moved.

use ahash::{AHashMap, AHashSet};
use manifold_core::change::{ChangeSet, Part, PartStamps};
use manifold_core::project::Project;
use manifold_core::scene_doc::ModelFacts;
use manifold_core::LayerId;

/// What moves when a document can come to name a new file.
const NAMES_MOVE: ChangeSet = ChangeSet::of(&[Part::Layers, Part::Shapes]);

#[derive(Default)]
pub(crate) struct ModelStore {
    known: AHashMap<String, ModelFacts>,
    /// Files sent to the worker: read, being read, or unreadable. None is
    /// sent twice.
    asked: AHashSet<String>,
    /// The stamp of the last sweep.
    swept: Option<u64>,
}

/// What a sweep found: facts to put on documents now, and files to read.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct Sweep {
    pub attach: Vec<(String, ModelFacts)>,
    pub read: Vec<String>,
}

impl ModelStore {
    /// `path`'s facts, read.
    pub(crate) fn arrived(&mut self, path: String, facts: ModelFacts) {
        self.asked.insert(path.clone());
        self.known.insert(path, facts);
    }

    /// The files scene documents name without their facts, when a layer or
    /// an instance's structure moved since the last sweep.
    pub(crate) fn sweep(&mut self, project: &Project, stamps: &PartStamps) -> Option<Sweep> {
        let stamp = stamps.key(NAMES_MOVE);
        if self.swept == Some(stamp) {
            return None;
        }
        self.swept = Some(stamp);
        let mut sweep = Sweep::default();
        let (known, asked) = (&self.known, &mut self.asked);
        for layer in &project.timeline.layers {
            let Some(doc) = layer.gen_params().and_then(|inst| inst.scene.as_ref()) else {
                continue;
            };
            doc.model_files(|path| {
                if path.is_empty() || doc.models.contains(path) {
                    return;
                }
                match known.get(path) {
                    Some(facts) => {
                        if !sweep.attach.iter().any(|(known, _)| known == path) {
                            sweep.attach.push((path.to_owned(), facts.clone()));
                        }
                    }
                    None => {
                        if asked.insert(path.to_owned()) {
                            sweep.read.push(path.to_owned());
                        }
                    }
                }
            });
        }
        (sweep != Sweep::default()).then_some(sweep)
    }
}

/// Put `path`'s facts on every scene document that names the file and lacks
/// them, recompiling each; with `layer`, on that layer's document even
/// before it names the file, so an import about to add it compiles once.
/// Both threads run it, as one project write.
pub(crate) fn attach(project: &mut Project, layer: Option<&LayerId>, path: &str, facts: &ModelFacts) {
    for host in &mut project.timeline.layers {
        let target = layer == Some(&host.layer_id);
        let Some(inst) = host.gen_params_mut() else {
            continue;
        };
        let Some(doc) = inst.scene.as_ref().filter(|doc| !doc.models.contains(path)) else {
            continue;
        };
        let mut names = false;
        doc.model_files(|file| names |= file == path);
        if !names && !target {
            continue;
        }
        let mut doc = doc.clone();
        doc.models.insert(path.to_owned(), facts.clone());
        if !names {
            // Nothing reads the file yet: no recompile.
            inst.scene = Some(doc);
        } else if let Err(error) = inst.install_scene_document(doc) {
            log::warn!("[scene] {path}: its layer did not recompile with the file's facts: {error}");
        }
    }
}

/// Read each file's facts on a worker thread, one event per file.
pub(crate) fn read_on_worker(paths: Vec<String>, tx: crossbeam_channel::Sender<crate::import_worker::ImportProgress>) {
    let spawned = std::thread::Builder::new().name("manifold-model-facts".into()).spawn(move || {
        for path in paths {
            let read = manifold_nodes_scene::node_graph::gltf_import::read_facts(std::path::Path::new(&path));
            if tx.send(crate::import_worker::ImportProgress::Facts { path, read }).is_err() {
                return;
            }
        }
    });
    if let Err(error) = spawned {
        log::warn!("[scene] could not start the model facts reader: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::scene_doc::{Item, ItemFields, ItemId, MeshPrimitives, ObjectFields, Shape};
    use manifold_core::types::LayerType;
    use manifold_core::PresetTypeId;

    /// A scene layer whose document draws `file`, without its facts.
    fn naming(file: &str) -> (Project, LayerId) {
        let mut project = Project::default();
        let index = project.timeline.add_layer("Scene", LayerType::Generator, PresetTypeId::SCENE);
        let layer = project.timeline.layers[index].layer_id.clone();
        let inst = project.timeline.layers[index].gen_params_mut().expect("a scene layer");
        let mut doc = inst.scene.clone().expect("a scene document");
        let shape = Shape::Mesh { file: file.into(), node_path: "part".into(), vertex_count: 3, primitives: MeshPrimitives::All };
        doc.items.push(Item::new(ItemId::new("part"), "Part", ItemFields::Object(ObjectFields { shape, ..ObjectFields::default() })));
        inst.install_scene_document(doc).expect("the scene compiles");
        (project, layer)
    }

    fn doc_has(project: &Project, file: &str) -> bool {
        project.timeline.layers[0].gen_params().and_then(|inst| inst.scene.as_ref()).is_some_and(|doc| doc.models.contains(file))
    }

    /// A file a document names without facts is read once; once read, every
    /// document naming it gets the facts, after any later structure move.
    #[test]
    fn a_named_file_is_read_once_and_its_facts_reach_the_document() {
        let (mut project, _) = naming("a.glb");
        let mut store = ModelStore::default();
        let mut stamps = PartStamps::default();
        let sweep = store.sweep(&project, &stamps).expect("the file is missing");
        assert_eq!(sweep.read, ["a.glb"]);
        assert!(store.sweep(&project, &stamps).is_none(), "nothing moved");
        stamps.bump(ChangeSet::ALL);
        assert!(store.sweep(&project, &stamps).is_none(), "a file is asked for once");

        let facts = ModelFacts::new(());
        store.arrived("a.glb".into(), facts.clone());
        stamps.bump(NAMES_MOVE);
        let sweep = store.sweep(&project, &stamps).expect("the facts are known");
        assert_eq!(sweep.attach, [("a.glb".to_owned(), facts.clone())]);
        attach(&mut project, None, "a.glb", &facts);
        assert!(doc_has(&project, "a.glb"));
    }

    /// An import's facts go on its layer before the file is added, so the
    /// add compiles once, with them.
    #[test]
    fn an_imports_facts_go_on_its_layer_before_the_file() {
        let (mut project, layer) = naming("a.glb");
        attach(&mut project, Some(&layer), "b.glb", &ModelFacts::new(()));
        assert!(doc_has(&project, "b.glb"));
        attach(&mut project, None, "c.glb", &ModelFacts::new(()));
        assert!(!doc_has(&project, "c.glb"), "a document that names no such file takes nothing");
    }
}
