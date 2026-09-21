//! Logical content identity for resources flowing through the node graph.
//!
//! A physical storage slot may be recycled between unrelated logical
//! resources. These tokens let consumers distinguish storage freshness from
//! the content of the logical resource they are reading.

use crate::node_graph::execution_plan::ResourceId;

/// Monotonic freshness of one physical storage slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StorageRevision(pub(crate) u64);

/// Identity and logical content revision of one resource publication.
/// Physical write counters cannot be used as logical content versions:
///
/// ```compile_fail
/// use manifold_renderer::node_graph::{ContentVersion, StorageRevision};
/// fn appearance_version(storage: StorageRevision) -> ContentVersion { storage }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContentVersion {
    epoch: u64,
    resource: ResourceId,
    revision: u64,
}

impl ContentVersion {
    pub(crate) fn new(epoch: u64, resource: ResourceId, revision: u64) -> Self {
        Self { epoch, resource, revision }
    }
}
