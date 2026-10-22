//! What the model files a scene document names say, read once on the import
//! worker and carried with the document, so no compile on the UI or content
//! thread opens a file (SCENE_DOCUMENT_DESIGN.md section 7.7 (Imports)).
//!
//! The facts are opaque here: the scene crate owns their shape, as it owns
//! the compiler ([`super::SceneCompiler`]), and reads them back by type. A
//! file whose facts have not arrived compiles as a plain mesh with the
//! default surface until they do. They are never saved: a load reads them
//! again on the worker.

use std::any::Any;
use std::collections::BTreeMap;
use std::panic::RefUnwindSafe;
use std::sync::Arc;

/// One model file's facts, shared and never changed once read.
#[derive(Clone)]
pub struct ModelFacts(Arc<dyn Any + Send + Sync + RefUnwindSafe>);

impl ModelFacts {
    pub fn new<T: Any + Send + Sync + RefUnwindSafe>(facts: T) -> Self {
        Self(Arc::new(facts))
    }

    /// The facts as the type the scene crate stored.
    pub fn get<T: Any>(&self) -> Option<&T> {
        let facts: &dyn Any = &*self.0;
        facts.downcast_ref()
    }
}

/// Two reads of one file are the same facts only when they are one read.
impl PartialEq for ModelFacts {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for ModelFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ModelFacts")
    }
}

/// The facts of a document's model files, by path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneModels(BTreeMap<String, ModelFacts>);

impl SceneModels {
    pub fn get(&self, path: &str) -> Option<&ModelFacts> {
        self.0.get(path)
    }

    pub fn contains(&self, path: &str) -> bool {
        self.0.contains_key(path)
    }

    pub fn insert(&mut self, path: String, facts: ModelFacts) {
        self.0.insert(path, facts);
    }

    /// Take every file's facts from `other`, the newer document: an undo back
    /// to an older document keeps what was read since, a re-read included.
    pub fn keep_from(&mut self, other: &SceneModels) {
        for (path, facts) in &other.0 {
            self.0.insert(path.clone(), facts.clone());
        }
    }
}
