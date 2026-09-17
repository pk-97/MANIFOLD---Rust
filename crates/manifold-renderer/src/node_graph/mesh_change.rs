//! Mesh revision metadata for automatic scene-modifier ray tracing.
//!
//! Types are specified verbatim in `docs/SCENE_MODIFIER_RT_DESIGN.md`
//! section 3.1 (exact types and ownership) and section 3.3 (fusion and
//! graph loading, owned prepared forms). Renderer-owned; `manifold-gpu`
//! does not depend on them. No serde derives — the prepared forms are
//! not serialized into `EffectGraphDef`, WGSL, or project files.

use std::borrow::Cow;

/// Which mesh property an output's revision tracks. See design §3.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeshAspect {
    /// Triangle connectivity (index/vertex count, index values).
    Topology,
    /// Vertex position values.
    Positions,
    /// Any other vertex payload (normals, UVs, morph deltas, cut maps).
    Content,
}

/// A named input port and the aspect of it this output depends on.
/// See design §3.1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshDependency {
    /// Input port name; `Cow::Borrowed` in static declarations,
    /// `Cow::Owned` in fused declarations.
    pub input: Cow<'static, str>,
    /// Aspect of the named input this output revises on.
    pub aspect: MeshAspect,
}

/// How an output aspect's revision advances. See design §3.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeshRevisionRule<'a> {
    /// Revise after every actual write; honor memo skips and truthful
    /// `mark_outputs_unchanged`.
    Written,
    /// Stable across writes while resource identity/layout and executor
    /// epoch are stable — only for primitives guaranteeing fixed
    /// triangle connectivity independent of all live controls.
    Fixed,
    /// Revise when any named input aspect changes; an empty list is
    /// `Fixed`. Input `Content` covers non-mesh controls such as cut
    /// maps.
    Dependencies(&'a [MeshDependency]),
}

/// Per-output revision rules for the two structural aspects that gate
/// acceleration-structure work. See design §3.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshOutputRule<'a> {
    /// Rule for triangle connectivity.
    pub topology: MeshRevisionRule<'a>,
    /// Rule for vertex positions.
    pub positions: MeshRevisionRule<'a>,
}

/// Monotonic revision counters for one mesh resource. See design §3.1.
/// Position changes also imply content changes; structural
/// identity/layout changes advance all three.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeshRevision {
    /// Triangle connectivity revision.
    pub topology: u64,
    /// Vertex position revision.
    pub positions: u64,
    /// Any vertex payload revision; advances on every actual output
    /// write, including normals/UV-only changes.
    pub content: u64,
}

impl MeshRevision {
    /// The counter for one aspect — the executor's dependency snapshots
    /// index by aspect rather than duplicating the match everywhere.
    pub fn aspect(&self, aspect: MeshAspect) -> u64 {
        match aspect {
            MeshAspect::Topology => self.topology,
            MeshAspect::Positions => self.positions,
            MeshAspect::Content => self.content,
        }
    }
}

/// Owned form of [`MeshRevisionRule`] for prepared/fused graphs. See
/// design §3.3 — fusion composes rules at preparation time, so fused
/// declarations own their dependency lists (`Cow::Owned` names).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreparedMeshRevisionRule {
    /// Revise after every actual write.
    Written,
    /// Stable across writes while identity/layout and epoch are stable.
    Fixed,
    /// Revise when any named input aspect changes.
    Dependencies(Vec<MeshDependency>),
}

impl PreparedMeshRevisionRule {
    /// Borrowing view for plan compilation: `Written`→`Written`,
    /// `Fixed`→`Fixed`, `Dependencies(vec)`→`Dependencies(&vec[..])`.
    /// The prepared form owns its dependency list; the compiled rule
    /// borrows it, so this conversion must stay a faithful re-borrow.
    pub fn as_borrowed(&self) -> MeshRevisionRule<'_> {
        match self {
            PreparedMeshRevisionRule::Written => MeshRevisionRule::Written,
            PreparedMeshRevisionRule::Fixed => MeshRevisionRule::Fixed,
            PreparedMeshRevisionRule::Dependencies(deps) => MeshRevisionRule::Dependencies(deps),
        }
    }
}

/// Owned per-output rule with the output port name, for prepared/fused
/// graphs. See design §3.3.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedMeshOutputRule {
    /// Final output port name of the prepared node.
    pub output: String,
    /// Rule for triangle connectivity.
    pub topology: PreparedMeshRevisionRule,
    /// Rule for vertex positions.
    pub positions: PreparedMeshRevisionRule,
}

/// Mesh rules for a whole prepared graph, keyed by generated node ID.
/// See design §3.3. Unfused views use an empty map; node declarations
/// supply their rules; cache keys must include a mesh-rule schema
/// revision.
pub type PreparedMeshRules =
    ahash::AHashMap<manifold_core::NodeId, Vec<PreparedMeshOutputRule>>;
