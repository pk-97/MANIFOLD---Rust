//! App-wide build-time feature toggles shared by the engine and the UI.
//!
//! These are plain `const bool`s (not registries or model types), so they live
//! in `foundation` where both `manifold-core` and `manifold-ui` can read the
//! same single source — the UI cannot depend on `manifold-core`.

/// Master kill-switch for the "3D Shading" (depth-relight) feature
/// (`docs/DEPTH_RELIGHT_DESIGN.md`). While `false`, the relight stage is never
/// compiled into any effect/generator graph and the "3D Shading" card UI (the
/// header "3D" toggle + the six knobs + Height From) is hidden — regardless of
/// a `PresetInstance`'s stored `relight` flag, so projects that saved
/// `relight: true` also render inert and no project can silently re-enable it.
/// Stored `relight`/`relight_params` are preserved on disk; they simply have no
/// effect while this is off. Flip to `true` to re-enable the whole surface
/// (renderer + UI) in one place.
pub const RELIGHT_FEATURE_ENABLED: bool = false;
