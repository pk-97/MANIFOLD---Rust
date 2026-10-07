//! Guard observations for the owning expansion fixture.
impl super::PreparedModifierParameterGuards {
    #[doc(hidden)]
    pub(crate) fn test_scenes(&self) -> &[manifold_core::NodeId] { &self.scenes }
    #[doc(hidden)]
    pub(crate) fn test_sources_empty(&self) -> bool { self.sources.is_empty() }
}
