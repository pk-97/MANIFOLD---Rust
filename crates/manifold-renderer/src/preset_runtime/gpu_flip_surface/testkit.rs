//! Shared-input contract of the surface preparation fixture.
impl crate::preset_runtime::PresetRuntime {
    #[doc(hidden)]
    pub(crate) fn test_surface_inputs() -> &'static [&'static str] { super::SHARED_INPUTS }
}
