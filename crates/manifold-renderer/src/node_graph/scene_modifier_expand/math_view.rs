use manifold_core::NodeId;

/// Original embedded-view semantics, used only by migrated Scope macros.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyMathViewScope { ThisModifier, WithinChain }

/// Internal request passed through the canonical scene-modifier builder. The
/// requested modifier is the standalone Math View instance; the derived graph
/// evaluates every preceding modifier of the same scene on sampled real faces.
#[derive(Debug, Clone, Copy)]
pub(super) struct MathViewRequest<'a> {
    pub(super) modifier_id: &'a NodeId,
    pub(super) legacy_scope: Option<LegacyMathViewScope>,
}
